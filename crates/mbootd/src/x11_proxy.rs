use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;

pub const HELPER_ARGUMENT: &str = "--x11-proxy";
pub(crate) const HELPER_PATH: &str = "/home/user/.mochios-x11-proxy";
const FDS_ENV: &str = "MOCHIOS_X11_PROXY_FDS";
const MAX_CONNECTIONS: usize = 32;

pub(crate) struct ProxyPool {
    child_fds: Vec<UnixStream>,
}

impl ProxyPool {
    pub(crate) fn new(host_socket: &Path) -> io::Result<Self> {
        let mut child_fds = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            let (host, child) = UnixStream::pair()?;
            set_close_on_exec(child.as_raw_fd(), false)?;
            let socket = host_socket.to_path_buf();
            thread::spawn(move || relay_host_connection(host, &socket));
            child_fds.push(child);
        }
        Ok(Self { child_fds })
    }

    pub(crate) fn environment_value(&self) -> String {
        self.child_fds
            .iter()
            .map(|stream| stream.as_raw_fd().to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn relay_host_connection(mut bridge: UnixStream, host_socket: &Path) {
    let mut activation = [0u8; 1];
    if bridge.read_exact(&mut activation).is_err() || activation[0] != 1 {
        return;
    }
    let Ok(host) = UnixStream::connect(host_socket) else {
        return;
    };
    relay(bridge, host);
}

pub fn run_helper(entrypoint: &str) -> io::Result<ExitStatus> {
    let display = std::env::var("DISPLAY")
        .ok()
        .and_then(|value| display_socket(&value))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid DISPLAY"))?;
    let raw_fds = parse_fds(&std::env::var(FDS_ENV).unwrap_or_default())?;
    let mut bridges = Vec::with_capacity(raw_fds.len());
    for fd in raw_fds {
        set_close_on_exec(fd, true)?;
        // SAFETY: the descriptors were explicitly inherited for this helper and
        // are unique entries in a validated list.
        bridges.push(unsafe { UnixStream::from_raw_fd(fd) });
    }
    if bridges.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no X11 bridges"));
    }
    if let Some(parent) = display.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::remove_file(&display) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&display)?;
    let mut permissions = fs::metadata(&display)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o777);
    fs::set_permissions(&display, permissions)?;
    let acceptor = thread::spawn(move || {
        for bridge in bridges {
            let Ok((client, _)) = listener.accept() else {
                break;
            };
            thread::spawn(move || {
                let mut bridge = bridge;
                if bridge.write_all(&[1]).is_ok() {
                    relay(client, bridge);
                }
            });
        }
    });
    let status = Command::new(entrypoint).status();
    let _ = fs::remove_file(&display);
    drop(acceptor);
    status
}

fn relay(left: UnixStream, right: UnixStream) {
    let Ok(mut left_read) = left.try_clone() else {
        return;
    };
    let Ok(mut right_write) = right.try_clone() else {
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut left_read, &mut right_write);
        let _ = right_write.shutdown(std::net::Shutdown::Write);
    });
    let mut right_read = right;
    let mut left_write = left;
    let _ = io::copy(&mut right_read, &mut left_write);
    let _ = left_write.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

fn display_socket(display: &str) -> Option<PathBuf> {
    let number = display.strip_prefix(':')?.split('.').next()?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
}

fn parse_fds(value: &str) -> io::Result<Vec<RawFd>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    for part in value.split(',') {
        let fd = part
            .parse::<RawFd>()
            .ok()
            .filter(|fd| *fd >= 3)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid bridge fd"))?;
        if output.contains(&fd) || output.len() >= MAX_CONNECTIONS {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid bridge fd list"));
        }
        output.push(fd);
    }
    Ok(output)
}

fn set_close_on_exec(fd: RawFd, enabled: bool) -> io::Result<()> {
    // SAFETY: fcntl does not retain pointers and fd is owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let updated = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: same descriptor validity argument as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, updated) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_map_only_to_local_unix_sockets() {
        assert_eq!(display_socket(":1001"), Some(PathBuf::from("/tmp/.X11-unix/X1001")));
        assert_eq!(display_socket(":1001.0"), Some(PathBuf::from("/tmp/.X11-unix/X1001")));
        assert_eq!(display_socket("tcp/host:0"), None);
        assert_eq!(display_socket(":../1"), None);
    }

    #[test]
    fn inherited_descriptor_list_is_bounded_and_unique() {
        assert_eq!(parse_fds("3,4,99").unwrap(), vec![3, 4, 99]);
        assert!(parse_fds("3,3").is_err());
        assert!(parse_fds("2").is_err());
        assert!(parse_fds("3,evil").is_err());
    }
}
