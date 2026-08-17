use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const CONTROL_ROOT: &str = "/run/wpa_supplicant";
const CLIENT_ROOT: &str = "/run/mboot";
const MAX_CONTROL_REPLY: usize = 64 * 1024;
const MAX_SCAN_RESULTS: usize = 24;
static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WifiStatus {
    pub(crate) available: bool,
    pub(crate) enabled: bool,
    pub(crate) connected: bool,
    pub(crate) interface: String,
    pub(crate) ssid: Vec<u8>,
    pub(crate) address: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WifiNetwork {
    pub(crate) ssid: Vec<u8>,
    pub(crate) signal: i32,
    pub(crate) secured: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WifiError {
    Unavailable,
    InvalidArgument,
    Rejected,
    Internal,
}

#[derive(Default)]
pub(crate) struct WifiManager;

impl WifiManager {
    pub(crate) fn status(&self) -> Result<WifiStatus, WifiError> {
        let Some(interface) = wireless_interface()? else {
            return Ok(WifiStatus {
                available: false,
                enabled: false,
                connected: false,
                interface: String::new(),
                ssid: Vec::new(),
                address: String::new(),
            });
        };
        let enabled = interface_enabled(&interface).unwrap_or(false);
        let response = control(&interface, "STATUS").unwrap_or_default();
        let state = status_value(&response, "wpa_state").unwrap_or_default();
        let address = ipv4_address(&interface).unwrap_or_default();
        Ok(WifiStatus {
            available: true,
            enabled,
            connected: state == "COMPLETED",
            interface,
            ssid: status_value(&response, "ssid")
                .map(decode_wpa_text)
                .unwrap_or_default(),
            address,
        })
    }

    pub(crate) fn scan(&self) -> Result<Vec<WifiNetwork>, WifiError> {
        let Some(interface) = wireless_interface()? else {
            return Ok(Vec::new());
        };
        if control(&interface, "SCAN").is_ok_and(|response| response.trim() == "OK") {
            std::thread::sleep(Duration::from_millis(800));
        }
        let response = control(&interface, "SCAN_RESULTS")?;
        Ok(parse_scan_results(&response))
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> Result<(), WifiError> {
        let interface = required_interface()?;
        if enabled {
            set_link(&interface, true)?;
            expect_ok(&control(&interface, "REASSOCIATE")?)
        } else {
            expect_ok(&control(&interface, "DISCONNECT")?)?;
            set_link(&interface, false)
        }
    }

    pub(crate) fn connect(&self, ssid: &[u8], credential: Option<&[u8]>) -> Result<(), WifiError> {
        if ssid.is_empty() || ssid.len() > 32 {
            return Err(WifiError::InvalidArgument);
        }
        let interface = required_interface()?;
        set_link(&interface, true)?;
        let _ = control(&interface, "REMOVE_NETWORK all");
        let network = control(&interface, "ADD_NETWORK")?;
        let network = network.trim();
        if network.is_empty() || !network.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(WifiError::Rejected);
        }
        expect_ok(&control(
            &interface,
            &format!("SET_NETWORK {network} ssid {}", encode_hex(ssid)),
        )?)?;
        match credential {
            Some(credential) => {
                let credential =
                    std::str::from_utf8(credential).map_err(|_| WifiError::InvalidArgument)?;
                if !(8..=63).contains(&credential.len())
                    || credential.chars().any(char::is_control)
                {
                    return Err(WifiError::InvalidArgument);
                }
                expect_ok(&control(
                    &interface,
                    &format!(
                        "SET_NETWORK {network} psk \"{}\"",
                        escape_quoted(credential)
                    ),
                )?)?;
                expect_ok(&control(
                    &interface,
                    &format!("SET_NETWORK {network} key_mgmt WPA-PSK SAE"),
                )?)?;
            }
            None => expect_ok(&control(
                &interface,
                &format!("SET_NETWORK {network} key_mgmt NONE"),
            )?)?,
        }
        expect_ok(&control(&interface, &format!("ENABLE_NETWORK {network}"))?)?;
        expect_ok(&control(&interface, &format!("SELECT_NETWORK {network}"))?)?;
        expect_ok(&control(&interface, "SAVE_CONFIG")?)?;
        start_dhcp(&interface)
    }

    pub(crate) fn disconnect(&self) -> Result<(), WifiError> {
        let interface = required_interface()?;
        expect_ok(&control(&interface, "DISCONNECT")?)
    }
}

fn wireless_interface() -> Result<Option<String>, WifiError> {
    let entries = fs::read_dir("/sys/class/net").map_err(|_| WifiError::Internal)?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if valid_interface(name) && entry.path().join("wireless").is_dir() {
            return Ok(Some(name.to_owned()));
        }
    }
    Ok(None)
}

fn required_interface() -> Result<String, WifiError> {
    wireless_interface()?.ok_or(WifiError::Unavailable)
}

fn valid_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn interface_enabled(interface: &str) -> io::Result<bool> {
    Ok(fs::read_to_string(format!("/sys/class/net/{interface}/operstate"))?.trim() != "down")
}

fn ipv4_address(interface: &str) -> Option<String> {
    let output = Command::new("/sbin/ip")
        .args(["-4", "-o", "addr", "show", "dev", interface])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = std::str::from_utf8(&output.stdout).ok()?;
    text.split_whitespace()
        .find(|field| field.contains('.') && field.contains('/'))
        .and_then(|field| field.split('/').next())
        .map(str::to_owned)
}

fn set_link(interface: &str, enabled: bool) -> Result<(), WifiError> {
    let status = Command::new("/sbin/ip")
        .args([
            "link",
            "set",
            "dev",
            interface,
            if enabled { "up" } else { "down" },
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| WifiError::Internal)?;
    if status.success() {
        Ok(())
    } else {
        Err(WifiError::Rejected)
    }
}

fn start_dhcp(interface: &str) -> Result<(), WifiError> {
    let pidfile = format!("/run/mboot/udhcpc-{interface}.pid");
    Command::new("/sbin/udhcpc")
        .args(["-b", "-q", "-i", interface, "-p", &pidfile])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| WifiError::Internal)
}

fn control(interface: &str, command: &str) -> Result<String, WifiError> {
    if !valid_interface(interface) || command.is_empty() || command.contains(['\n', '\r']) {
        return Err(WifiError::InvalidArgument);
    }
    fs::create_dir_all(CLIENT_ROOT).map_err(|_| WifiError::Internal)?;
    let server = PathBuf::from(CONTROL_ROOT).join(interface);
    let is_socket = fs::metadata(&server)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false);
    if !is_socket {
        return Err(WifiError::Unavailable);
    }
    let client_path = PathBuf::from(format!(
        "{CLIENT_ROOT}/wpa-ctl-{}-{}",
        std::process::id(),
        CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup = SocketPath(client_path.clone());
    let socket = UnixDatagram::bind(&client_path).map_err(|_| WifiError::Internal)?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| WifiError::Internal)?;
    socket.connect(server).map_err(|_| WifiError::Unavailable)?;
    socket
        .send(command.as_bytes())
        .map_err(|_| WifiError::Internal)?;
    let mut reply = vec![0u8; MAX_CONTROL_REPLY];
    let length = socket.recv(&mut reply).map_err(|_| WifiError::Rejected)?;
    reply.truncate(length);
    String::from_utf8(reply).map_err(|_| WifiError::Rejected)
}

struct SocketPath(PathBuf);

impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn expect_ok(response: &str) -> Result<(), WifiError> {
    if response.trim() == "OK" {
        Ok(())
    } else {
        Err(WifiError::Rejected)
    }
}

fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == key).then_some(value)
    })
}

fn parse_scan_results(input: &str) -> Vec<WifiNetwork> {
    let mut networks = Vec::<WifiNetwork>::new();
    for line in input.lines().skip(1) {
        let mut fields = line.splitn(5, '\t');
        let (Some(_bssid), Some(_frequency), Some(signal), Some(flags), Some(ssid)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let ssid = decode_wpa_text(ssid);
        if ssid.is_empty() || ssid.len() > 32 {
            continue;
        }
        let Ok(signal) = signal.parse::<i32>() else {
            continue;
        };
        let secured = flags.contains("WPA") || flags.contains("RSN") || flags.contains("SAE");
        if let Some(existing) = networks.iter_mut().find(|network| network.ssid == ssid) {
            if signal > existing.signal {
                existing.signal = signal;
                existing.secured = secured;
            }
            continue;
        }
        networks.push(WifiNetwork {
            ssid,
            signal,
            secured,
        });
    }
    networks.sort_by(|left, right| right.signal.cmp(&left.signal));
    networks.truncate(MAX_SCAN_RESULTS);
    networks
}

fn decode_wpa_text(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() && bytes[index + 1] == b'x' {
            if let (Some(high), Some(low)) = (hex(bytes[index + 2]), hex(bytes[index + 3])) {
                output.push((high << 4) | low);
                index += 4;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    output
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn decode_hex(value: &str, maximum: usize) -> Result<Vec<u8>, WifiError> {
    if value.len() % 2 != 0 || value.len() > maximum.saturating_mul(2) {
        return Err(WifiError::InvalidArgument);
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex(pair[0]).ok_or(WifiError::InvalidArgument)?;
        let low = hex(pair[1]).ok_or(WifiError::InvalidArgument)?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn escape_quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_results_are_deduplicated_sorted_and_classified() {
        let input = "bssid / frequency / signal level / flags / ssid\naa\t2412\t-70\t[WPA2-PSK-CCMP][ESS]\tHome\nbb\t2412\t-40\t[WPA2-PSK-CCMP][ESS]\tHome\ncc\t5180\t-50\t[ESS]\tGuest\\x20WiFi\n";
        assert_eq!(
            parse_scan_results(input),
            vec![
                WifiNetwork {
                    ssid: b"Home".to_vec(),
                    signal: -40,
                    secured: true
                },
                WifiNetwork {
                    ssid: b"Guest WiFi".to_vec(),
                    signal: -50,
                    secured: false
                },
            ]
        );
    }

    #[test]
    fn credential_encoding_is_bounded_and_reversible() {
        assert_eq!(decode_hex("6d6f6368694f53", 32).unwrap(), b"mochiOS");
        assert_eq!(decode_hex("0", 32), Err(WifiError::InvalidArgument));
        assert_eq!(escape_quoted("a\\\"b"), "a\\\\\\\"b");
    }
}
