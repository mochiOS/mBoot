use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::linux_portal::PortalMount;
use crate::linux_stage::valid_bundle_id;
use crate::x11_proxy::{HELPER_ARGUMENT, HELPER_PATH, ProxyPool};

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
    helper_placeholder: Option<PathBuf>,
    network: Option<NetworkLease>,
}

struct NetworkLease {
    host_interface: String,
    rules: Vec<(Option<&'static str>, &'static str, Vec<String>)>,
}

impl LinuxSandbox {
    pub(crate) fn prepare(
        instance: u64,
        bundle: &str,
        user: &str,
        writable: &str,
        network: &str,
        portal_mounts: &[PortalMount],
    ) -> Result<Self, SandboxError> {
        if instance == 0
            || !valid_bundle_id(bundle)
            || !valid_user(user)
            || !matches!(network, "none" | "client")
        {
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
            helper_placeholder: None,
            network: None,
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
        sandbox.mount_devices(&merged)?;
        sandbox.mount_tmp(&merged)?;
        if network == "client" {
            sandbox.mount_resolver(&merged)?;
        }
        sandbox.mount_mochios(&merged, portal_mounts)?;
        sandbox.mount_runtime_helper(&merged)?;
        Ok(sandbox)
    }

    pub(crate) fn launch(
        &mut self,
        entrypoint: &str,
        display: &str,
        instance: u64,
        host_x11_socket: &Path,
        network: &str,
    ) -> Result<Child, SandboxError> {
        if !valid_absolute_path(entrypoint)
            || !self.root.join("root").join(&entrypoint[1..]).is_file()
        {
            return Err(SandboxError::NotFound);
        }
        let root = self.root.join("root");
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(format!("/var/log/mboot/linux-{instance}.log"))
            .map_err(|_| SandboxError::Internal)?;
        let stdout = log.try_clone().map_err(|_| SandboxError::Internal)?;
        let proxy = ProxyPool::new(host_x11_socket).map_err(|_| SandboxError::Internal)?;
        let mut child = Command::new("unshare")
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
            .arg(HELPER_PATH)
            .arg(HELPER_ARGUMENT)
            .arg(entrypoint)
            .env_clear()
            .env("DISPLAY", display)
            .env("HOME", "/home/user")
            .env("USER", "user")
            .env("LOGNAME", "user")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("TMPDIR", "/tmp")
            .env("MOCHIOS_LINUX_INSTANCE", instance.to_string())
            .env("MOCHIOS_X11_PROXY_FDS", proxy.environment_value())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|_| SandboxError::Internal)?;
        drop(proxy);
        if network == "client" {
            match NetworkLease::configure(instance, child.id()) {
                Ok(lease) => self.network = Some(lease),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        Ok(child)
    }

    fn mount_runtime_helper(&mut self, merged: &Path) -> Result<(), SandboxError> {
        let source = std::env::current_exe().map_err(|_| SandboxError::Internal)?;
        let destination = merged.join(HELPER_PATH.trim_start_matches('/'));
        fs::write(&destination, []).map_err(|_| SandboxError::Internal)?;
        run_mount(["--bind", path_str(&source)?, path_str(&destination)?])?;
        self.mounts.push(destination);
        self.helper_placeholder = self.mounts.last().cloned();
        Ok(())
    }

    fn mount_resolver(&mut self, merged: &Path) -> Result<(), SandboxError> {
        let destination = merged.join("etc/resolv.conf");
        if !destination.is_file() {
            return Err(SandboxError::NotFound);
        }
        let source = self.root.join("resolv.conf");
        fs::write(&source, b"nameserver 1.1.1.1\nnameserver 9.9.9.9\n")
            .map_err(|_| SandboxError::Internal)?;
        run_mount(["--bind", path_str(&source)?, path_str(&destination)?])?;
        self.mounts.push(destination.clone());
        run_mount(["-o", "remount,bind,ro,nodev,nosuid,noexec", path_str(&destination)?])
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

    fn mount_devices(&mut self, merged: &Path) -> Result<(), SandboxError> {
        let target = merged.join("dev");
        if !target.is_dir() {
            return Err(SandboxError::NotFound);
        }
        run_mount([
            "-t",
            "tmpfs",
            "-o",
            "nosuid,noexec,mode=0755,size=64K",
            "tmpfs",
            path_str(&target)?,
        ])?;
        self.mounts.push(target.clone());
        for name in ["null", "zero", "full", "random", "urandom"] {
            let source = Path::new("/dev").join(name);
            if !fs::metadata(&source)
                .map(|metadata| metadata.file_type().is_char_device())
                .unwrap_or(false)
            {
                return Err(SandboxError::NotFound);
            }
            let destination = target.join(name);
            fs::write(&destination, []).map_err(|_| SandboxError::Internal)?;
            run_mount(["--bind", path_str(&source)?, path_str(&destination)?])?;
            self.mounts.push(destination);
        }
        let shared_memory = target.join("shm");
        fs::create_dir(&shared_memory).map_err(|_| SandboxError::Internal)?;
        run_mount([
            "-t",
            "tmpfs",
            "-o",
            "nodev,nosuid,noexec,mode=1777,size=512M",
            "tmpfs",
            path_str(&shared_memory)?,
        ])?;
        self.mounts.push(shared_memory);
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

impl NetworkLease {
    fn configure(instance: u64, namespace_pid: u32) -> Result<Self, SandboxError> {
        let suffix = format!("{:08x}", instance as u32);
        let host_interface = format!("mh{suffix}");
        let guest_interface = format!("mg{suffix}");
        let slot = (instance % 16_384) as u32;
        let address = (100u32 << 24) | (64u32 << 16) | (slot * 4);
        let octets = |value: u32| format!("{}.{}.{}.{}", value >> 24, (value >> 16) & 255, (value >> 8) & 255, value & 255);
        let subnet = format!("{}/30", octets(address));
        let host_address = format!("{}/30", octets(address + 1));
        let guest_address = format!("{}/30", octets(address + 2));
        let gateway = octets(address + 1);
        let mut lease = Self { host_interface: host_interface.clone(), rules: Vec::new() };

        run_command("ip", &["link", "add", &host_interface, "type", "veth", "peer", "name", &guest_interface])?;
        if let Err(error) = (|| {
            run_command("ip", &["link", "set", &guest_interface, "netns", &namespace_pid.to_string()])?;
            run_command("ip", &["addr", "add", &host_address, "dev", &host_interface])?;
            run_command("ip", &["link", "set", &host_interface, "up"])?;
            let namespace = format!("/proc/{namespace_pid}/ns/net");
            run_command("nsenter", &["--net", &namespace, "--", "ip", "link", "set", "lo", "up"])?;
            run_command("nsenter", &["--net", &namespace, "--", "ip", "addr", "add", &guest_address, "dev", &guest_interface])?;
            run_command("nsenter", &["--net", &namespace, "--", "ip", "link", "set", &guest_interface, "up"])?;
            run_command("nsenter", &["--net", &namespace, "--", "ip", "route", "add", "default", "via", &gateway])?;
            fs::write("/proc/sys/net/ipv4/ip_forward", b"1\n").map_err(|_| SandboxError::Internal)?;
            for destination in [
                "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8",
                "169.254.0.0/16", "172.16.0.0/12", "192.0.0.0/24", "192.168.0.0/16",
                "198.18.0.0/15", "224.0.0.0/4", "240.0.0.0/4",
            ] {
                lease.add_rule(None, "FORWARD", &["-i", &host_interface, "-d", destination, "-j", "REJECT"])?;
            }
            lease.add_rule(None, "INPUT", &["-i", &host_interface, "-j", "DROP"])?;
            lease.add_rule(None, "FORWARD", &["-i", &host_interface, "-j", "ACCEPT"])?;
            lease.add_rule(None, "FORWARD", &["-o", &host_interface, "-m", "conntrack", "--ctstate", "RELATED,ESTABLISHED", "-j", "ACCEPT"])?;
            lease.add_rule(None, "FORWARD", &["-o", &host_interface, "-j", "DROP"])?;
            lease.add_rule(Some("nat"), "POSTROUTING", &["-s", &subnet, "-j", "MASQUERADE"])?;
            Ok(())
        })() {
            drop(lease);
            return Err(error);
        }
        Ok(lease)
    }

    fn add_rule(&mut self, table: Option<&'static str>, chain: &'static str, specification: &[&str]) -> Result<(), SandboxError> {
        let mut arguments = Vec::new();
        if let Some(table) = table {
            arguments.extend(["-t".to_owned(), table.to_owned()]);
        }
        arguments.extend(["-A".to_owned(), chain.to_owned()]);
        arguments.extend(specification.iter().map(|value| (*value).to_owned()));
        run_owned_command("iptables", &arguments)?;
        self.rules.push((table, chain, specification.iter().map(|value| (*value).to_owned()).collect()));
        Ok(())
    }
}

impl Drop for NetworkLease {
    fn drop(&mut self) {
        for (table, chain, specification) in self.rules.iter().rev() {
            let mut arguments = Vec::new();
            if let Some(table) = table {
                arguments.extend(["-t".to_owned(), (*table).to_owned()]);
            }
            arguments.extend(["-D".to_owned(), (*chain).to_owned()]);
            arguments.extend(specification.iter().cloned());
            let _ = run_owned_command("iptables", &arguments);
        }
        let _ = run_command("ip", &["link", "del", &self.host_interface]);
    }
}

impl Drop for LinuxSandbox {
    fn drop(&mut self) {
        for mount in self.mounts.iter().rev() {
            let _ = Command::new("umount").arg("-l").arg(mount).status();
        }
        if let Some(path) = &self.helper_placeholder {
            let _ = fs::remove_file(path);
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

fn run_command(program: &str, arguments: &[&str]) -> Result<(), SandboxError> {
    Command::new(program)
        .args(arguments)
        .status()
        .map_err(|_| SandboxError::Internal)?
        .success()
        .then_some(())
        .ok_or(SandboxError::Internal)
}

fn run_owned_command(program: &str, arguments: &[String]) -> Result<(), SandboxError> {
    Command::new(program)
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
