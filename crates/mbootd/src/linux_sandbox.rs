use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::linux_portal::PortalMount;
use crate::linux_stage::valid_bundle_id;

const INSTANCE_ROOT: &str = "/run/mboot/linux";
const PACKAGE_ROOT: &str = "/bin/mboot";
const USER_ROOT: &str = "/libraries/users";
const SANDBOX_UID: &str = "65534";
const SANDBOX_GID: &str = "65534";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SandboxError {
    InvalidArgument,
    NotFound,
    Internal,
}

pub(crate) struct LinuxSandbox {
    root: PathBuf,
    mounts: Vec<PathBuf>,
}

impl LinuxSandbox {
    pub(crate) fn prepare(
        instance: u64,
        bundle: &str,
        user: &str,
        writable: &str,
        portal_mounts: &[PortalMount],
    ) -> Result<Self, SandboxError> {
        if instance == 0 || !valid_bundle_id(bundle) || !valid_user(user) {
            return Err(SandboxError::InvalidArgument);
        }
        let paths = parse_writable_paths(writable)?;
        let image = Path::new(PACKAGE_ROOT).join(bundle).join("rootfs.squashfs");
        if !image.is_file() {
            return Err(SandboxError::NotFound);
        }
        let root = Path::new(INSTANCE_ROOT).join(instance.to_string());
        if root.exists() {
            return Err(SandboxError::InvalidArgument);
        }
        let lower = root.join("lower");
        let merged = root.join("root");
        fs::create_dir_all(&lower).map_err(|_| SandboxError::Internal)?;
        fs::create_dir_all(&merged).map_err(|_| SandboxError::Internal)?;

        let mut sandbox = Self {
            root,
            mounts: Vec::new(),
        };
        run_mount([
            "-t",
            "squashfs",
            "-o",
            "loop,ro,nodev,nosuid",
            path_str(&image)?,
            path_str(&lower)?,
        ])?;
        sandbox.mounts.push(lower.clone());
        run_mount(["--bind", path_str(&lower)?, path_str(&merged)?])?;
        sandbox.mounts.push(merged.clone());

        let storage = Path::new(USER_ROOT).join(user).join("mboot").join(bundle);
        fs::create_dir_all(&storage).map_err(|_| SandboxError::Internal)?;
        sandbox.mount_writable_path(&lower, &merged, &storage, "/home/user")?;
        for path in paths {
            sandbox.mount_writable_path(&lower, &merged, &storage, path)?;
        }
        sandbox.mount_tmp(&merged)?;
        sandbox.mount_mochios(&merged, portal_mounts)?;
        Ok(sandbox)
    }

    pub(crate) fn launch(
        &self,
        entrypoint: &str,
        display: &str,
        instance: u64,
    ) -> Result<Child, SandboxError> {
        if !valid_absolute_path(entrypoint)
            || !self.root.join("root").join(&entrypoint[1..]).is_file()
        {
            return Err(SandboxError::NotFound);
        }
        let root = self.root.join("root");
        Command::new("unshare")
            .args([
                "--mount",
                "--pid",
                "--fork",
                "--kill-child=SIGKILL",
                "--ipc",
                "--uts",
                "--net",
                "--root",
                path_str(&root)?,
                "--mount-proc",
                "--setuid",
                SANDBOX_UID,
                "--setgid",
                SANDBOX_GID,
                "--",
            ])
            .arg(entrypoint)
            .env_clear()
            .env("DISPLAY", display)
            .env("HOME", "/home/user")
            .env("USER", "user")
            .env("LOGNAME", "user")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("TMPDIR", "/tmp")
            .env("MOCHIOS_LINUX_INSTANCE", instance.to_string())
            .spawn()
            .map_err(|_| SandboxError::Internal)
    }

    pub(crate) fn expose_x11(&mut self, socket: &Path) -> Result<(), SandboxError> {
        if !fs::metadata(socket)
            .map(|metadata| metadata.file_type().is_socket())
            .unwrap_or(false)
        {
            return Err(SandboxError::NotFound);
        }
        let directory = self.root.join("root/tmp/.X11-unix");
        fs::create_dir_all(&directory).map_err(|_| SandboxError::Internal)?;
        let destination = directory.join(socket.file_name().ok_or(SandboxError::InvalidArgument)?);
        fs::write(&destination, []).map_err(|_| SandboxError::Internal)?;
        run_mount(["--bind", path_str(socket)?, path_str(&destination)?])?;
        self.mounts.push(destination);
        Ok(())
    }

    fn mount_writable_path(
        &mut self,
        lower: &Path,
        merged: &Path,
        storage: &Path,
        path: &str,
    ) -> Result<(), SandboxError> {
        let relative = &path[1..];
        let lower_path = lower.join(relative);
        let target = merged.join(relative);
        if !lower_path.is_dir() || !target.is_dir() {
            return Err(SandboxError::NotFound);
        }
        let key = storage_key(path);
        let upper = storage.join("rootfs-upper").join(&key);
        let work = storage.join("rootfs-work").join(&key);
        fs::create_dir_all(&upper).map_err(|_| SandboxError::Internal)?;
        fs::create_dir_all(&work).map_err(|_| SandboxError::Internal)?;
        let options = format!(
            "nodev,nosuid,lowerdir={},upperdir={},workdir={}",
            lower_path.display(),
            upper.display(),
            work.display()
        );
        run_mount([
            "-t",
            "overlay",
            "-o",
            &options,
            "overlay",
            path_str(&target)?,
        ])?;
        self.mounts.push(target);
        set_sandbox_owner(self.mounts.last().ok_or(SandboxError::Internal)?)?;
        Ok(())
    }

    fn mount_tmp(&mut self, merged: &Path) -> Result<(), SandboxError> {
        let target = merged.join("tmp");
        if !target.is_dir() {
            return Err(SandboxError::NotFound);
        }
        run_mount([
            "-t",
            "tmpfs",
            "-o",
            "nodev,nosuid,noexec,mode=1777,size=256M",
            "tmpfs",
            path_str(&target)?,
        ])?;
        self.mounts.push(target);
        Ok(())
    }

    fn mount_mochios(
        &mut self,
        merged: &Path,
        portal_mounts: &[PortalMount],
    ) -> Result<(), SandboxError> {
        let target = merged.join("mochios");
        if !target.is_dir() {
            return Err(SandboxError::NotFound);
        }
        run_mount([
            "-t",
            "tmpfs",
            "-o",
            "nodev,nosuid,noexec,mode=0555,size=64K",
            "tmpfs",
            path_str(&target)?,
        ])?;
        self.mounts.push(target.clone());
        for portal in portal_mounts {
            if !valid_absolute_path(&portal.target) || !portal.source.is_dir() {
                return Err(SandboxError::InvalidArgument);
            }
            if portal.writable {
                set_sandbox_owner(&portal.source)?;
            }
            let destination = target.join(&portal.target[1..]);
            fs::create_dir_all(&destination).map_err(|_| SandboxError::Internal)?;
            run_mount(["--bind", path_str(&portal.source)?, path_str(&destination)?])?;
            self.mounts.push(destination.clone());
            if !portal.writable {
                run_mount([
                    "-o",
                    "remount,bind,ro,nodev,nosuid,noexec",
                    path_str(&destination)?,
                ])?;
            }
        }
        run_mount(["-o", "remount,ro,nodev,nosuid,noexec", path_str(&target)?])?;
        Ok(())
    }
}

impl Drop for LinuxSandbox {
    fn drop(&mut self) {
        for mount in self.mounts.iter().rev() {
            let _ = Command::new("umount").arg("-l").arg(mount).status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_mount<const N: usize>(arguments: [&str; N]) -> Result<(), SandboxError> {
    Command::new("mount")
        .args(arguments)
        .status()
        .map_err(|_| SandboxError::Internal)?
        .success()
        .then_some(())
        .ok_or(SandboxError::Internal)
}

fn set_sandbox_owner(path: &Path) -> Result<(), SandboxError> {
    Command::new("chown")
        .args(["-R", &format!("{SANDBOX_UID}:{SANDBOX_GID}")])
        .arg(path)
        .status()
        .map_err(|_| SandboxError::Internal)?
        .success()
        .then_some(())
        .ok_or(SandboxError::Internal)
}

fn path_str(path: &Path) -> Result<&str, SandboxError> {
    path.to_str().ok_or(SandboxError::InvalidArgument)
}

fn valid_user(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn valid_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains("//")
        && path[1..]
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn valid_writable_path(path: &str) -> bool {
    valid_absolute_path(path)
        && !["/dev", "/proc", "/sys", "/run", "/tmp", "/home", "/mochios"]
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}/")))
        && !matches!(
            path,
            "/bin" | "/sbin" | "/lib" | "/lib64" | "/usr" | "/usr/bin" | "/usr/lib"
        )
        && !path.starts_with("/bin/")
        && !path.starts_with("/sbin/")
        && !path.starts_with("/lib/")
        && !path.starts_with("/lib64/")
        && !path.starts_with("/usr/bin/")
        && !path.starts_with("/usr/lib/")
        && !path.starts_with("/mochios")
}

fn parse_writable_paths(value: &str) -> Result<Vec<&str>, SandboxError> {
    if value == "none" {
        return Ok(Vec::new());
    }
    let paths = value.split(',').collect::<Vec<_>>();
    if paths.is_empty()
        || paths.len() > 32
        || paths.iter().any(|path| !valid_writable_path(path))
        || paths.iter().enumerate().any(|(index, path)| {
            paths[index + 1..]
                .iter()
                .any(|other| paths_overlap(path, other))
        })
    {
        return Err(SandboxError::InvalidArgument);
    }
    Ok(paths)
}

fn storage_key(path: &str) -> String {
    let mut key = String::with_capacity(path.len() * 2);
    for byte in path.bytes() {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_paths_exclude_executable_and_mochios_trees() {
        assert!(parse_writable_paths("/usr/share/editor,/var/lib/editor").is_ok());
        assert!(parse_writable_paths("none").unwrap().is_empty());
        for path in [
            "/usr/bin/editor",
            "/usr/lib/editor",
            "/mochios/system",
            "/home/user",
            "/tmp/cache",
            "/proc/self",
            "/var/../lib",
        ] {
            assert_eq!(
                parse_writable_paths(path),
                Err(SandboxError::InvalidArgument)
            );
        }
        assert_eq!(
            parse_writable_paths("/var/lib/editor,/var/lib/editor/cache"),
            Err(SandboxError::InvalidArgument)
        );
        assert_ne!(storage_key("/a_b"), storage_key("/a/b"));
    }

    #[test]
    fn user_and_entrypoint_validation_is_strict() {
        assert!(valid_user("alice-1"));
        assert!(!valid_user("../root"));
        assert!(valid_absolute_path("/usr/bin/editor"));
        assert!(!valid_absolute_path("/usr/bin/../sh"));
    }
}
