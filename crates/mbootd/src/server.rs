use crate::developer::{encode_hex, DeveloperBuildState, DeveloperError};
use crate::linux::{LinuxBridge, LinuxError};
use crate::{GuestState, StateError};
use mboot_protocol::{
    decode_line, encode_to_string, Argument, Body, Command, ConnectionValidator, Destination,
    ErrorCode, KnownCommand, Message, MessageType, ValidationError, MAX_MESSAGE_LEN,
};
use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_SOCKET_PATH: &str = "/run/mboot/mochios-control.sock";
const DEFAULT_HEARTBEAT_MS: u64 = 5_000;
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn run(path: &Path) -> io::Result<()> {
    prepare_socket(path)?;
    let listener = UnixListener::bind(path)?;
    let _socket = SocketFile::new(path)?;
    println!("mbootd listening: {}", path.display());
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let mut state = GuestState::default();
                if let Err(error) = serve_connection(stream, &mut state, DEFAULT_HEARTBEAT_MS) {
                    eprintln!("guest connection failed: {error}");
                }
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    Ok(())
}

pub fn run_one(path: &Path, state: &mut GuestState) -> io::Result<()> {
    prepare_socket(path)?;
    let listener = UnixListener::bind(path)?;
    let _socket = SocketFile::new(path)?;
    let (stream, _) = listener.accept()?;
    serve_connection(stream, state, DEFAULT_HEARTBEAT_MS)
}

struct SocketFile {
    path: PathBuf,
}

impl SocketFile {
    fn new(path: &Path) -> io::Result<Self> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            path: path.to_owned(),
        })
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn serve_connection(
    mut stream: UnixStream,
    state: &mut GuestState,
    heartbeat_ms: u64,
) -> io::Result<()> {
    let read_stream = stream.try_clone()?;
    read_stream.set_read_timeout(Some(Duration::from_millis(heartbeat_ms)))?;
    let mut reader = BufReader::new(read_stream);
    let mut validator = ConnectionValidator::new();
    let mut developer = DeveloperBuildState::default();
    let mut linux = LinuxBridge::default();
    let session = session_id();
    state.connected();
    println!("guest connected");

    loop {
        let line = match read_line_limited(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if state.check_heartbeat_timeout(
                    Duration::from_millis(heartbeat_ms.saturating_mul(2)),
                    Instant::now(),
                ) {
                    println!("guest heartbeat timed out");
                }
                continue;
            }
            Err(error) => return Err(error),
        };

        let message = match decode_line(&line) {
            Ok(message) => message,
            Err(error) => {
                eprintln!("invalid protocol message: {error}");
                continue;
            }
        };
        if let Err(error) = validator.accept(&message) {
            if message.message_type == MessageType::Request {
                send_validation_error(&mut stream, &message, error)?;
            }
            continue;
        }

        let response = dispatch(
            state,
            &mut developer,
            &mut linux,
            &message,
            &session,
            heartbeat_ms,
        );
        if let Some(response) = response {
            send(&mut stream, &response)?;
            if message.message_type == MessageType::Request {
                validator
                    .complete(message.request_id)
                    .map_err(protocol_io_error)?;
            }
        }
    }
}

fn dispatch(
    state: &mut GuestState,
    developer: &mut DeveloperBuildState,
    linux: &mut LinuxBridge,
    message: &Message,
    session: &str,
    heartbeat_ms: u64,
) -> Option<Message> {
    let command = match &message.body {
        Body::Command(Command::Known(command)) => *command,
        Body::Command(Command::Unsupported(_)) => {
            return request_error(message, ErrorCode::Unsupported, None)
        }
        Body::Ok | Body::Error(_) => return None,
    };
    match command {
        KnownCommand::ProtocolHello => match state.negotiate(message) {
            Ok(()) => {
                println!("protocol negotiated: version=1");
                Some(Message::command(
                    Destination::Mochios,
                    MessageType::Response,
                    message.request_id,
                    KnownCommand::ProtocolWelcome,
                    vec![
                        Argument::new("version", "1"),
                        Argument::new("session", session),
                        Argument::new("heartbeat_ms", heartbeat_ms.to_string()),
                    ],
                ))
            }
            Err(error) => request_error(message, state_error_code(error), state_error_field(error)),
        },
        KnownCommand::ProtocolSync => {
            println!("protocol synchronized");
            Some(Message::ok(
                Destination::Mochios,
                message.request_id,
                Vec::new(),
            ))
        }
        KnownCommand::ProtocolPing => Some(Message::ok(
            Destination::Mochios,
            message.request_id,
            Vec::new(),
        )),
        KnownCommand::GuestReady => match state.ready(message) {
            Ok(stage) => {
                println!("guest boot stage: {}", stage.as_str());
                None
            }
            Err(error) => {
                eprintln!("invalid GUEST.READY: {error}");
                None
            }
        },
        KnownCommand::GuestHeartbeat => match state.heartbeat(message) {
            Ok(uptime_ms) => {
                println!("guest heartbeat: uptime={uptime_ms}ms");
                None
            }
            Err(error) => {
                eprintln!("invalid GUEST.HEARTBEAT: {error}");
                None
            }
        },
        KnownCommand::GuestStopping => {
            if let Err(error) = state.stopping() {
                eprintln!("invalid GUEST.STOPPING: {error}");
            }
            None
        }
        KnownCommand::GuestPanic => {
            eprintln!("guest reported panic");
            None
        }
        KnownCommand::HostStatus => Some(Message::ok(
            Destination::Mochios,
            message.request_id,
            state.status_arguments(),
        )),
        KnownCommand::HostPoweroff | KnownCommand::HostReboot => {
            request_error(message, ErrorCode::Unsupported, None)
        }
        KnownCommand::DeveloperBegin => developer_response(message, || {
            developer.begin(
                required_u64(message, "transaction")?,
                required_u64(message, "size")?,
            )?;
            Ok(Vec::new())
        }),
        KnownCommand::DeveloperChunk => developer_response(message, || {
            developer.append_chunk(
                required_u64(message, "transaction")?,
                required_u64(message, "offset")?,
                message
                    .argument("data")
                    .ok_or(DeveloperError::InvalidArgument)?,
            )?;
            Ok(Vec::new())
        }),
        KnownCommand::DeveloperCompile => developer_response(message, || {
            let result = developer.compile(required_u64(message, "transaction")?)?;
            Ok(vec![
                Argument::new("status", result.status.to_string()),
                Argument::new("output_size", result.output_size.to_string()),
                Argument::new("diagnostics_size", result.diagnostics_size.to_string()),
            ])
        }),
        KnownCommand::DeveloperRead => developer_response(message, || {
            let result = developer.read(
                required_u64(message, "transaction")?,
                message
                    .argument("stream")
                    .ok_or(DeveloperError::InvalidArgument)?,
                required_u64(message, "offset")?,
                required_u64(message, "maximum")?,
            )?;
            Ok(vec![
                Argument::new("total_size", result.total_size.to_string()),
                Argument::new("data", encode_hex(&result.data)),
            ])
        }),
        KnownCommand::DeveloperCancel => developer_response(message, || {
            developer.cancel(required_u64(message, "transaction")?)?;
            Ok(Vec::new())
        }),
        KnownCommand::LinuxLaunch => linux_response(message, || {
            let process = linux.launch(
                linux_u64(message, "instance")?,
                message
                    .argument("application")
                    .ok_or(LinuxError::InvalidArgument)?,
            )?;
            Ok(vec![Argument::new("process", process.to_string())])
        }),
        KnownCommand::LinuxWindows => linux_response(message, || {
            let windows = linux.windows(linux_u64(message, "instance")?)?;
            let list = if windows.is_empty() {
                String::from("none")
            } else {
                windows
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            Ok(vec![Argument::new("windows", list)])
        }),
        KnownCommand::LinuxWindowInfo => linux_response(message, || {
            let info =
                linux.window_info(linux_u64(message, "instance")?, linux_window(message)?)?;
            Ok(vec![
                Argument::new("width", info.width.to_string()),
                Argument::new("height", info.height.to_string()),
                Argument::new("generation", info.generation.to_string()),
                Argument::new("frame_size", info.frame_size.to_string()),
                Argument::new("title", encode_hex(&info.title)),
            ])
        }),
        KnownCommand::LinuxFrame => linux_response(message, || {
            let chunk = linux.frame(
                linux_u64(message, "instance")?,
                linux_window(message)?,
                linux_u64(message, "generation")?,
                linux_u64(message, "offset")?,
                linux_u64(message, "maximum")?,
            )?;
            Ok(vec![
                Argument::new("total_size", chunk.total_size.to_string()),
                Argument::new("data", encode_hex(&chunk.bytes)),
            ])
        }),
        KnownCommand::LinuxInput => linux_response(message, || {
            linux.input(
                linux_u64(message, "instance")?,
                linux_window(message)?,
                message
                    .argument("kind")
                    .ok_or(LinuxError::InvalidArgument)?,
                linux_u8(message, "code")?,
                linux_i32(message, "value")?,
                linux_i16(message, "x")?,
                linux_i16(message, "y")?,
            )?;
            Ok(Vec::new())
        }),
        KnownCommand::LinuxConfigure => linux_response(message, || {
            linux.configure(
                linux_u64(message, "instance")?,
                linux_window(message)?,
                linux_u16(message, "width")?,
                linux_u16(message, "height")?,
            )?;
            Ok(Vec::new())
        }),
        KnownCommand::LinuxClose => linux_response(message, || {
            linux.close(linux_u64(message, "instance")?, linux_window(message)?)?;
            Ok(Vec::new())
        }),
        KnownCommand::ProtocolWelcome
        | KnownCommand::GuestStatus
        | KnownCommand::GuestShutdown
        | KnownCommand::GuestReboot => request_error(message, ErrorCode::InvalidCommand, None),
    }
}

fn developer_response(
    message: &Message,
    operation: impl FnOnce() -> Result<Vec<Argument>, DeveloperError>,
) -> Option<Message> {
    match operation() {
        Ok(arguments) => Some(Message::ok(
            Destination::Mochios,
            message.request_id,
            arguments,
        )),
        Err(error) => request_error(message, developer_error_code(error), None),
    }
}

fn required_u64(message: &Message, key: &str) -> Result<u64, DeveloperError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(DeveloperError::InvalidArgument)
}

fn developer_error_code(error: DeveloperError) -> ErrorCode {
    match error {
        DeveloperError::Busy => ErrorCode::Busy,
        DeveloperError::InvalidArgument => ErrorCode::InvalidArgument,
        DeveloperError::InvalidState => ErrorCode::InvalidState,
        DeveloperError::Internal => ErrorCode::Internal,
    }
}

fn linux_response(
    message: &Message,
    operation: impl FnOnce() -> Result<Vec<Argument>, LinuxError>,
) -> Option<Message> {
    match operation() {
        Ok(arguments) => Some(Message::ok(
            Destination::Mochios,
            message.request_id,
            arguments,
        )),
        Err(error) => request_error(message, linux_error_code(error), None),
    }
}

fn linux_u64(message: &Message, key: &str) -> Result<u64, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_u32(message: &Message, key: &str) -> Result<u32, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_u16(message: &Message, key: &str) -> Result<u16, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_u8(message: &Message, key: &str) -> Result<u8, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_i32(message: &Message, key: &str) -> Result<i32, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_i16(message: &Message, key: &str) -> Result<i16, LinuxError> {
    message
        .argument(key)
        .and_then(|value| value.parse().ok())
        .ok_or(LinuxError::InvalidArgument)
}

fn linux_window(message: &Message) -> Result<u32, LinuxError> {
    linux_u32(message, "window")
}

fn linux_error_code(error: LinuxError) -> ErrorCode {
    match error {
        LinuxError::Busy => ErrorCode::Busy,
        LinuxError::InvalidArgument => ErrorCode::InvalidArgument,
        LinuxError::InvalidState | LinuxError::NotFound => ErrorCode::InvalidState,
        LinuxError::Internal => ErrorCode::Internal,
    }
}

fn request_error(
    request: &Message,
    code: ErrorCode,
    field: Option<&'static str>,
) -> Option<Message> {
    if request.message_type != MessageType::Request {
        return None;
    }
    let arguments = field
        .map(|field| vec![Argument::new("field", field)])
        .unwrap_or_default();
    Some(Message::error(
        Destination::Mochios,
        request.request_id,
        code,
        arguments,
    ))
}

fn send_validation_error(
    stream: &mut UnixStream,
    request: &Message,
    error: ValidationError,
) -> io::Result<()> {
    let code = match error {
        ValidationError::UnsupportedCommand => ErrorCode::Unsupported,
        ValidationError::InvalidState => ErrorCode::InvalidState,
        ValidationError::TooManyPendingRequests | ValidationError::RequestIdInUse => {
            ErrorCode::Busy
        }
        ValidationError::InvalidArgument | ValidationError::DuplicateArgument => {
            ErrorCode::InvalidArgument
        }
        ValidationError::InvalidVersion
        | ValidationError::InvalidRequestId
        | ValidationError::InvalidMessageType
        | ValidationError::InvalidDirection
        | ValidationError::RequestIdNotPending => ErrorCode::InvalidCommand,
    };
    if let Some(response) = request_error(request, code, None) {
        send(stream, &response)?;
    }
    Ok(())
}

fn state_error_code(error: StateError) -> ErrorCode {
    match error {
        StateError::MissingArgument(_) | StateError::InvalidArgument(_) => {
            ErrorCode::InvalidArgument
        }
        StateError::InvalidState => ErrorCode::InvalidState,
    }
}

fn state_error_field(error: StateError) -> Option<&'static str> {
    match error {
        StateError::MissingArgument(field) | StateError::InvalidArgument(field) => Some(field),
        StateError::InvalidState => None,
    }
}

fn send(stream: &mut UnixStream, message: &Message) -> io::Result<()> {
    let encoded = encode_to_string(message).map_err(protocol_io_error)?;
    stream.write_all(encoded.as_bytes())?;
    stream.flush()
}

fn read_line_limited(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if line.len() + take > MAX_MESSAGE_LEN {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "protocol line exceeds 4096 bytes",
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.ends_with(b"\n") {
            return Ok(Some(line));
        }
    }
}

fn prepare_socket(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => match UnixStream::connect(path) {
            Ok(_) => Err(io::Error::new(
                ErrorKind::AddrInUse,
                "control socket already has a listener",
            )),
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => fs::remove_file(path),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        Ok(_) => Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "control socket path exists and is not a socket",
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    let sequence = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:016x}{sequence:016x}")
}

fn protocol_io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConnectionState;

    #[test]
    fn limited_reader_rejects_oversized_line() {
        let bytes = vec![b'a'; MAX_MESSAGE_LEN + 1];
        let mut reader = BufReader::new(bytes.as_slice());
        let error = read_line_limited(&mut reader).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn host_actions_do_not_execute_system_commands() {
        let mut state = GuestState {
            connection_state: ConnectionState::Negotiated,
            ..GuestState::default()
        };
        let mut developer = DeveloperBuildState::default();
        let mut linux = LinuxBridge::default();
        for command in [KnownCommand::HostPoweroff, KnownCommand::HostReboot] {
            let message = Message::command(
                Destination::Mboot,
                MessageType::Request,
                2,
                command,
                Vec::new(),
            );
            assert!(matches!(
                dispatch(
                    &mut state,
                    &mut developer,
                    &mut linux,
                    &message,
                    "session",
                    5000,
                ),
                Some(Message {
                    body: Body::Error(ErrorCode::Unsupported),
                    ..
                })
            ));
        }
    }

    #[test]
    fn each_connection_gets_a_distinct_session_id() {
        assert_ne!(session_id(), session_id());
    }
}
