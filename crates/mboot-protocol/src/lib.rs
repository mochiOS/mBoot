#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

pub const VERSION: u16 = 1;
pub const MAX_MESSAGE_LEN: usize = 4096;
pub const MAX_PENDING_REQUESTS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Destination {
    Mboot,
    Mochios,
}

impl Destination {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mboot => "@MBOOT",
            Self::Mochios => "@MOCHIOS",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageType {
    Request,
    Response,
    Event,
}

impl MessageType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "REQ",
            Self::Response => "RES",
            Self::Event => "EVT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnownCommand {
    ProtocolSync,
    ProtocolHello,
    ProtocolWelcome,
    ProtocolPing,
    GuestReady,
    GuestHeartbeat,
    GuestStatus,
    GuestStopping,
    GuestShutdown,
    GuestReboot,
    GuestPanic,
    HostStatus,
    HostPoweroff,
    HostReboot,
    DeveloperBegin,
    DeveloperChunk,
    DeveloperCompile,
    DeveloperRead,
    DeveloperCancel,
    LinuxLaunch,
    LinuxStageBegin,
    LinuxStageChunk,
    LinuxStageCommit,
    LinuxStageCancel,
    LinuxPortalReset,
    LinuxPortalGrant,
    LinuxPortalMkdir,
    LinuxPortalFileBegin,
    LinuxPortalFileChunk,
    LinuxPortalFileCommit,
    LinuxPortalFileCancel,
    LinuxPortalRelease,
    LinuxPortalExportBegin,
    LinuxPortalExportEntry,
    LinuxPortalExportChunk,
    LinuxPortalExportEnd,
    LinuxBundleLaunch,
    LinuxWindows,
    LinuxWindowInfo,
    LinuxFrame,
    LinuxInput,
    LinuxConfigure,
    LinuxClose,
    WifiStatus,
    WifiScan,
    WifiSetEnabled,
    WifiConnect,
    WifiDisconnect,
}

impl KnownCommand {
    pub const ALL: [Self; 48] = [
        Self::ProtocolSync,
        Self::ProtocolHello,
        Self::ProtocolWelcome,
        Self::ProtocolPing,
        Self::GuestReady,
        Self::GuestHeartbeat,
        Self::GuestStatus,
        Self::GuestStopping,
        Self::GuestShutdown,
        Self::GuestReboot,
        Self::GuestPanic,
        Self::HostStatus,
        Self::HostPoweroff,
        Self::HostReboot,
        Self::DeveloperBegin,
        Self::DeveloperChunk,
        Self::DeveloperCompile,
        Self::DeveloperRead,
        Self::DeveloperCancel,
        Self::LinuxLaunch,
        Self::LinuxStageBegin,
        Self::LinuxStageChunk,
        Self::LinuxStageCommit,
        Self::LinuxStageCancel,
        Self::LinuxPortalReset,
        Self::LinuxPortalGrant,
        Self::LinuxPortalMkdir,
        Self::LinuxPortalFileBegin,
        Self::LinuxPortalFileChunk,
        Self::LinuxPortalFileCommit,
        Self::LinuxPortalFileCancel,
        Self::LinuxPortalRelease,
        Self::LinuxPortalExportBegin,
        Self::LinuxPortalExportEntry,
        Self::LinuxPortalExportChunk,
        Self::LinuxPortalExportEnd,
        Self::LinuxBundleLaunch,
        Self::LinuxWindows,
        Self::LinuxWindowInfo,
        Self::LinuxFrame,
        Self::LinuxInput,
        Self::LinuxConfigure,
        Self::LinuxClose,
        Self::WifiStatus,
        Self::WifiScan,
        Self::WifiSetEnabled,
        Self::WifiConnect,
        Self::WifiDisconnect,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolSync => "PROTOCOL.SYNC",
            Self::ProtocolHello => "PROTOCOL.HELLO",
            Self::ProtocolWelcome => "PROTOCOL.WELCOME",
            Self::ProtocolPing => "PROTOCOL.PING",
            Self::GuestReady => "GUEST.READY",
            Self::GuestHeartbeat => "GUEST.HEARTBEAT",
            Self::GuestStatus => "GUEST.STATUS",
            Self::GuestStopping => "GUEST.STOPPING",
            Self::GuestShutdown => "GUEST.SHUTDOWN",
            Self::GuestReboot => "GUEST.REBOOT",
            Self::GuestPanic => "GUEST.PANIC",
            Self::HostStatus => "HOST.STATUS",
            Self::HostPoweroff => "HOST.POWEROFF",
            Self::HostReboot => "HOST.REBOOT",
            Self::DeveloperBegin => "DEVELOPER.BEGIN",
            Self::DeveloperChunk => "DEVELOPER.CHUNK",
            Self::DeveloperCompile => "DEVELOPER.COMPILE",
            Self::DeveloperRead => "DEVELOPER.READ",
            Self::DeveloperCancel => "DEVELOPER.CANCEL",
            Self::LinuxLaunch => "LINUX.LAUNCH",
            Self::LinuxStageBegin => "LINUX.STAGE.BEGIN",
            Self::LinuxStageChunk => "LINUX.STAGE.CHUNK",
            Self::LinuxStageCommit => "LINUX.STAGE.COMMIT",
            Self::LinuxStageCancel => "LINUX.STAGE.CANCEL",
            Self::LinuxPortalReset => "LINUX.PORTAL.RESET",
            Self::LinuxPortalGrant => "LINUX.PORTAL.GRANT",
            Self::LinuxPortalMkdir => "LINUX.PORTAL.MKDIR",
            Self::LinuxPortalFileBegin => "LINUX.PORTAL.FILE.BEGIN",
            Self::LinuxPortalFileChunk => "LINUX.PORTAL.FILE.CHUNK",
            Self::LinuxPortalFileCommit => "LINUX.PORTAL.FILE.COMMIT",
            Self::LinuxPortalFileCancel => "LINUX.PORTAL.FILE.CANCEL",
            Self::LinuxPortalRelease => "LINUX.PORTAL.RELEASE",
            Self::LinuxPortalExportBegin => "LINUX.PORTAL.EXPORT.BEGIN",
            Self::LinuxPortalExportEntry => "LINUX.PORTAL.EXPORT.ENTRY",
            Self::LinuxPortalExportChunk => "LINUX.PORTAL.EXPORT.CHUNK",
            Self::LinuxPortalExportEnd => "LINUX.PORTAL.EXPORT.END",
            Self::LinuxBundleLaunch => "LINUX.BUNDLE.LAUNCH",
            Self::LinuxWindows => "LINUX.WINDOWS",
            Self::LinuxWindowInfo => "LINUX.WINDOW.INFO",
            Self::LinuxFrame => "LINUX.FRAME",
            Self::LinuxInput => "LINUX.INPUT",
            Self::LinuxConfigure => "LINUX.CONFIGURE",
            Self::LinuxClose => "LINUX.CLOSE",
            Self::WifiStatus => "WIFI.STATUS",
            Self::WifiScan => "WIFI.SCAN",
            Self::WifiSetEnabled => "WIFI.SET.ENABLED",
            Self::WifiConnect => "WIFI.CONNECT",
            Self::WifiDisconnect => "WIFI.DISCONNECT",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "PROTOCOL.SYNC" => Self::ProtocolSync,
            "PROTOCOL.HELLO" => Self::ProtocolHello,
            "PROTOCOL.WELCOME" => Self::ProtocolWelcome,
            "PROTOCOL.PING" => Self::ProtocolPing,
            "GUEST.READY" => Self::GuestReady,
            "GUEST.HEARTBEAT" => Self::GuestHeartbeat,
            "GUEST.STATUS" => Self::GuestStatus,
            "GUEST.STOPPING" => Self::GuestStopping,
            "GUEST.SHUTDOWN" => Self::GuestShutdown,
            "GUEST.REBOOT" => Self::GuestReboot,
            "GUEST.PANIC" => Self::GuestPanic,
            "HOST.STATUS" => Self::HostStatus,
            "HOST.POWEROFF" => Self::HostPoweroff,
            "HOST.REBOOT" => Self::HostReboot,
            "DEVELOPER.BEGIN" => Self::DeveloperBegin,
            "DEVELOPER.CHUNK" => Self::DeveloperChunk,
            "DEVELOPER.COMPILE" => Self::DeveloperCompile,
            "DEVELOPER.READ" => Self::DeveloperRead,
            "DEVELOPER.CANCEL" => Self::DeveloperCancel,
            "LINUX.LAUNCH" => Self::LinuxLaunch,
            "LINUX.STAGE.BEGIN" => Self::LinuxStageBegin,
            "LINUX.STAGE.CHUNK" => Self::LinuxStageChunk,
            "LINUX.STAGE.COMMIT" => Self::LinuxStageCommit,
            "LINUX.STAGE.CANCEL" => Self::LinuxStageCancel,
            "LINUX.PORTAL.RESET" => Self::LinuxPortalReset,
            "LINUX.PORTAL.GRANT" => Self::LinuxPortalGrant,
            "LINUX.PORTAL.MKDIR" => Self::LinuxPortalMkdir,
            "LINUX.PORTAL.FILE.BEGIN" => Self::LinuxPortalFileBegin,
            "LINUX.PORTAL.FILE.CHUNK" => Self::LinuxPortalFileChunk,
            "LINUX.PORTAL.FILE.COMMIT" => Self::LinuxPortalFileCommit,
            "LINUX.PORTAL.FILE.CANCEL" => Self::LinuxPortalFileCancel,
            "LINUX.PORTAL.RELEASE" => Self::LinuxPortalRelease,
            "LINUX.PORTAL.EXPORT.BEGIN" => Self::LinuxPortalExportBegin,
            "LINUX.PORTAL.EXPORT.ENTRY" => Self::LinuxPortalExportEntry,
            "LINUX.PORTAL.EXPORT.CHUNK" => Self::LinuxPortalExportChunk,
            "LINUX.PORTAL.EXPORT.END" => Self::LinuxPortalExportEnd,
            "LINUX.BUNDLE.LAUNCH" => Self::LinuxBundleLaunch,
            "LINUX.WINDOWS" => Self::LinuxWindows,
            "LINUX.WINDOW.INFO" => Self::LinuxWindowInfo,
            "LINUX.FRAME" => Self::LinuxFrame,
            "LINUX.INPUT" => Self::LinuxInput,
            "LINUX.CONFIGURE" => Self::LinuxConfigure,
            "LINUX.CLOSE" => Self::LinuxClose,
            "WIFI.STATUS" => Self::WifiStatus,
            "WIFI.SCAN" => Self::WifiScan,
            "WIFI.SET.ENABLED" => Self::WifiSetEnabled,
            "WIFI.CONNECT" => Self::WifiConnect,
            "WIFI.DISCONNECT" => Self::WifiDisconnect,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Known(KnownCommand),
    Unsupported(String),
}

impl From<KnownCommand> for Command {
    fn from(value: KnownCommand) -> Self {
        Self::Known(value)
    }
}

impl Command {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(command) => command.as_str(),
            Self::Unsupported(command) => command,
        }
    }

    pub const fn known(&self) -> Option<KnownCommand> {
        match self {
            Self::Known(command) => Some(*command),
            Self::Unsupported(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    Unsupported,
    InvalidCommand,
    InvalidArgument,
    InvalidState,
    PermissionDenied,
    Busy,
    Timeout,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::InvalidCommand => "invalid_command",
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidState => "invalid_state",
            Self::PermissionDenied => "permission_denied",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "unsupported" => Self::Unsupported,
            "invalid_command" => Self::InvalidCommand,
            "invalid_argument" => Self::InvalidArgument,
            "invalid_state" => Self::InvalidState,
            "permission_denied" => Self::PermissionDenied,
            "busy" => Self::Busy,
            "timeout" => Self::Timeout,
            "internal" => Self::Internal,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Body {
    Command(Command),
    Ok,
    Error(ErrorCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Argument {
    pub key: String,
    pub value: String,
}

impl Argument {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub destination: Destination,
    pub version: u16,
    pub message_type: MessageType,
    pub request_id: u64,
    pub body: Body,
    pub arguments: Vec<Argument>,
}

impl Message {
    pub fn command(
        destination: Destination,
        message_type: MessageType,
        request_id: u64,
        command: KnownCommand,
        arguments: Vec<Argument>,
    ) -> Self {
        Self {
            destination,
            version: VERSION,
            message_type,
            request_id,
            body: Body::Command(command.into()),
            arguments,
        }
    }

    pub fn ok(destination: Destination, request_id: u64, arguments: Vec<Argument>) -> Self {
        Self {
            destination,
            version: VERSION,
            message_type: MessageType::Response,
            request_id,
            body: Body::Ok,
            arguments,
        }
    }

    pub fn error(
        destination: Destination,
        request_id: u64,
        code: ErrorCode,
        arguments: Vec<Argument>,
    ) -> Self {
        Self {
            destination,
            version: VERSION,
            message_type: MessageType::Response,
            request_id,
            body: Body::Error(code),
            arguments,
        }
    }

    pub const fn known_command(&self) -> Option<KnownCommand> {
        match &self.body {
            Body::Command(command) => command.known(),
            Body::Ok | Body::Error(_) => None,
        }
    }

    pub fn argument(&self, key: &str) -> Option<&str> {
        self.arguments
            .iter()
            .find(|argument| argument.key == key)
            .map(|argument| argument.value.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    TooLong,
    InvalidUtf8,
    MissingLineFeed,
    EmbeddedLineFeed,
    MissingField,
    UnknownDestination,
    InvalidVersion,
    UnknownMessageType,
    InvalidRequestId,
    InvalidBody,
    InvalidMessageType,
    InvalidDirection,
    InvalidArgument,
    DuplicateArgument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    InvalidMessage(ValidationError),
    BufferTooSmall { required: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    InvalidVersion,
    InvalidRequestId,
    InvalidMessageType,
    InvalidDirection,
    InvalidArgument,
    DuplicateArgument,
    UnsupportedCommand,
    InvalidState,
    TooManyPendingRequests,
    RequestIdInUse,
    RequestIdNotPending,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self)
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self)
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecodeError {}
#[cfg(feature = "std")]
impl std::error::Error for EncodeError {}
#[cfg(feature = "std")]
impl std::error::Error for ValidationError {}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'=')
}

fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'/' | b'+' | b'-' | b',')
        })
}

fn validate_arguments(arguments: &[Argument]) -> Result<(), ValidationError> {
    for (index, argument) in arguments.iter().enumerate() {
        if !valid_key(&argument.key) || !valid_value(&argument.value) {
            return Err(ValidationError::InvalidArgument);
        }
        if arguments[..index]
            .iter()
            .any(|existing| existing.key == argument.key)
        {
            return Err(ValidationError::DuplicateArgument);
        }
    }
    Ok(())
}

pub fn validate_message(message: &Message) -> Result<(), ValidationError> {
    if message.version != VERSION {
        return Err(ValidationError::InvalidVersion);
    }
    match message.message_type {
        MessageType::Event if message.request_id != 0 => {
            return Err(ValidationError::InvalidRequestId);
        }
        MessageType::Request if message.request_id == 0 => {
            return Err(ValidationError::InvalidRequestId);
        }
        MessageType::Response if message.request_id == 0 => {
            return Err(ValidationError::InvalidRequestId);
        }
        _ => {}
    }
    validate_arguments(&message.arguments)?;
    if matches!(message.body, Body::Error(_)) && message.argument("code").is_some() {
        return Err(ValidationError::DuplicateArgument);
    }

    let command = match &message.body {
        Body::Command(Command::Known(command)) => *command,
        Body::Command(Command::Unsupported(_)) => return Err(ValidationError::UnsupportedCommand),
        Body::Ok | Body::Error(_) => {
            return if message.message_type == MessageType::Response {
                Ok(())
            } else {
                Err(ValidationError::InvalidMessageType)
            };
        }
    };

    validate_command_arguments(command, message)?;

    let (destination, message_type) = command_contract(command);
    if message_type != message.message_type {
        return Err(ValidationError::InvalidMessageType);
    }
    if !destination.accepts(message.destination) {
        return Err(ValidationError::InvalidDirection);
    }
    Ok(())
}

fn validate_command_arguments(
    command: KnownCommand,
    message: &Message,
) -> Result<(), ValidationError> {
    match command {
        KnownCommand::ProtocolHello => {
            if message.argument("system") != Some("mochios")
                || message.argument("version").is_none()
                || message.argument("boot_id").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::ProtocolWelcome => {
            if message.argument("version") != Some("1")
                || message.argument("session").is_none()
                || parse_u64_argument(message, "heartbeat_ms").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::GuestReady => {
            let stage = message
                .argument("stage")
                .ok_or(ValidationError::InvalidArgument)?;
            if !matches!(
                stage,
                "firmware" | "kernel" | "userspace" | "display" | "desktop"
            ) {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::GuestHeartbeat if parse_u64_argument(message, "uptime_ms").is_none() => {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::DeveloperBegin
            if parse_u64_argument(message, "transaction").is_none()
                || parse_u64_argument(message, "size").is_none() =>
        {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::DeveloperChunk
            if parse_u64_argument(message, "transaction").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || message.argument("data").is_none() =>
        {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::DeveloperCompile | KnownCommand::DeveloperCancel
            if parse_u64_argument(message, "transaction").is_none() =>
        {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::DeveloperRead => {
            if parse_u64_argument(message, "transaction").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || parse_u64_argument(message, "maximum").is_none()
                || !matches!(message.argument("stream"), Some("output" | "diagnostics"))
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxLaunch => {
            if message.argument("application").is_none()
                || parse_u64_argument(message, "instance").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxStageBegin => {
            if parse_u64_argument(message, "instance").is_none()
                || message.argument("bundle").is_none()
                || parse_u64_argument(message, "size").is_none()
                || message.argument("digest").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxStageChunk => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || message.argument("data").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxStageCommit | KnownCommand::LinuxStageCancel
            if parse_u64_argument(message, "instance").is_none() =>
        {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::LinuxPortalReset
        | KnownCommand::LinuxPortalFileCommit
        | KnownCommand::LinuxPortalFileCancel
        | KnownCommand::LinuxPortalRelease
        | KnownCommand::LinuxPortalExportEnd
            if parse_u64_argument(message, "instance").is_none() =>
        {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::LinuxPortalGrant => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "grant").is_none()
                || !matches!(message.argument("access"), Some("read" | "write"))
                || message.argument("path").is_none()
                || parse_u64_argument(message, "mode").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalMkdir => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "grant").is_none()
                || message.argument("path").is_none()
                || parse_u64_argument(message, "mode").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalFileBegin => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "grant").is_none()
                || parse_u64_argument(message, "size").is_none()
                || message.argument("path").is_none()
                || parse_u64_argument(message, "mode").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalFileChunk => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || message.argument("data").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalExportBegin => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "grant").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalExportEntry => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "index").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxPortalExportChunk => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "index").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || parse_u64_argument(message, "maximum").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxBundleLaunch => {
            if parse_u64_argument(message, "instance").is_none()
                || message.argument("bundle").is_none()
                || message.argument("entry").is_none()
                || message.argument("user").is_none()
                || message.argument("writable").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxWindows if parse_u64_argument(message, "instance").is_none() => {
            return Err(ValidationError::InvalidArgument);
        }
        KnownCommand::LinuxWindowInfo | KnownCommand::LinuxClose | KnownCommand::LinuxConfigure => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "window").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
            if command == KnownCommand::LinuxConfigure
                && (parse_u64_argument(message, "width").is_none()
                    || parse_u64_argument(message, "height").is_none())
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxFrame => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "window").is_none()
                || parse_u64_argument(message, "generation").is_none()
                || parse_u64_argument(message, "offset").is_none()
                || parse_u64_argument(message, "maximum").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::LinuxInput => {
            if parse_u64_argument(message, "instance").is_none()
                || parse_u64_argument(message, "window").is_none()
                || !matches!(
                    message.argument("kind"),
                    Some("motion" | "button" | "key" | "scroll" | "focus")
                )
                || parse_u64_argument(message, "code").is_none()
                || parse_u64_argument(message, "value").is_none()
                || parse_u64_argument(message, "x").is_none()
                || parse_u64_argument(message, "y").is_none()
                || parse_u64_argument(message, "modifiers").is_none()
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::WifiStatus | KnownCommand::WifiScan | KnownCommand::WifiDisconnect => {
            if !message.arguments.is_empty() {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::WifiSetEnabled => {
            if message.arguments.len() != 1
                || !matches!(message.argument("enabled"), Some("0" | "1"))
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        KnownCommand::WifiConnect => {
            let security = message.argument("security");
            let credential = message.argument("credential");
            if !matches!(security, Some("open" | "secured"))
                || message
                    .argument("ssid")
                    .is_none_or(|ssid| !valid_hex(ssid, 1, 32))
                || (security == Some("open") && credential.is_some())
                || (security == Some("secured")
                    && credential.is_none_or(|value| !valid_hex(value, 1, 63)))
                || message.arguments.len() != if security == Some("open") { 2 } else { 3 }
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        _ => {}
    }
    Ok(())
}

fn valid_hex(value: &str, minimum_bytes: usize, maximum_bytes: usize) -> bool {
    value.len().is_multiple_of(2)
        && (minimum_bytes * 2..=maximum_bytes * 2).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_u64_argument(message: &Message, key: &str) -> Option<u64> {
    message.argument(key)?.parse::<u64>().ok()
}

#[derive(Clone, Copy)]
enum AllowedDestination {
    Mboot,
    Mochios,
    Either,
}

impl AllowedDestination {
    const fn accepts(self, destination: Destination) -> bool {
        matches!(
            (self, destination),
            (Self::Mboot, Destination::Mboot)
                | (Self::Mochios, Destination::Mochios)
                | (Self::Either, _)
        )
    }
}

const fn command_contract(command: KnownCommand) -> (AllowedDestination, MessageType) {
    use AllowedDestination::{Either, Mboot, Mochios};
    use KnownCommand::*;
    match command {
        ProtocolSync | ProtocolPing => (Either, MessageType::Request),
        ProtocolHello => (Mboot, MessageType::Request),
        ProtocolWelcome => (Mochios, MessageType::Response),
        GuestReady | GuestHeartbeat | GuestStopping | GuestPanic => (Mboot, MessageType::Event),
        GuestStatus | GuestShutdown | GuestReboot => (Mochios, MessageType::Request),
        HostStatus | HostPoweroff | HostReboot => (Mboot, MessageType::Request),
        DeveloperBegin
        | DeveloperChunk
        | DeveloperCompile
        | DeveloperRead
        | DeveloperCancel
        | LinuxLaunch
        | LinuxStageBegin
        | LinuxStageChunk
        | LinuxStageCommit
        | LinuxStageCancel
        | LinuxPortalReset
        | LinuxPortalGrant
        | LinuxPortalMkdir
        | LinuxPortalFileBegin
        | LinuxPortalFileChunk
        | LinuxPortalFileCommit
        | LinuxPortalFileCancel
        | LinuxPortalRelease
        | LinuxPortalExportBegin
        | LinuxPortalExportEntry
        | LinuxPortalExportChunk
        | LinuxPortalExportEnd
        | LinuxBundleLaunch
        | LinuxWindows
        | LinuxWindowInfo
        | LinuxFrame
        | LinuxInput
        | LinuxConfigure
        | LinuxClose
        | WifiStatus
        | WifiScan
        | WifiSetEnabled
        | WifiConnect
        | WifiDisconnect => (Mboot, MessageType::Request),
    }
}

pub fn decode_line(bytes: &[u8]) -> Result<Message, DecodeError> {
    if bytes.len() > MAX_MESSAGE_LEN {
        return Err(DecodeError::TooLong);
    }
    if !bytes.ends_with(b"\n") {
        return Err(DecodeError::MissingLineFeed);
    }
    let content = &bytes[..bytes.len() - 1];
    if content.contains(&b'\n') || content.contains(&b'\r') {
        return Err(DecodeError::EmbeddedLineFeed);
    }
    let line = core::str::from_utf8(content).map_err(|_| DecodeError::InvalidUtf8)?;
    let mut fields = line.split(' ');
    let destination = match fields.next().ok_or(DecodeError::MissingField)? {
        "@MBOOT" => Destination::Mboot,
        "@MOCHIOS" => Destination::Mochios,
        _ => return Err(DecodeError::UnknownDestination),
    };
    let version = fields
        .next()
        .ok_or(DecodeError::MissingField)?
        .parse::<u16>()
        .map_err(|_| DecodeError::InvalidVersion)?;
    if version != VERSION {
        return Err(DecodeError::InvalidVersion);
    }
    let message_type = match fields.next().ok_or(DecodeError::MissingField)? {
        "REQ" => MessageType::Request,
        "RES" => MessageType::Response,
        "EVT" => MessageType::Event,
        _ => return Err(DecodeError::UnknownMessageType),
    };
    let request_id = fields
        .next()
        .ok_or(DecodeError::MissingField)?
        .parse::<u64>()
        .map_err(|_| DecodeError::InvalidRequestId)?;
    let body_text = fields.next().ok_or(DecodeError::MissingField)?;
    if body_text.is_empty() {
        return Err(DecodeError::MissingField);
    }
    let body = match body_text {
        "OK" => Body::Ok,
        "ERROR" => Body::Error(ErrorCode::Internal),
        command => Body::Command(match KnownCommand::parse(command) {
            Some(command) => Command::Known(command),
            None => Command::Unsupported(command.to_string()),
        }),
    };
    let mut arguments = Vec::new();
    for field in fields {
        if field.is_empty() {
            return Err(DecodeError::InvalidArgument);
        }
        let (key, value) = field.split_once('=').ok_or(DecodeError::InvalidArgument)?;
        if !valid_key(key) || !valid_value(value) {
            return Err(DecodeError::InvalidArgument);
        }
        if arguments
            .iter()
            .any(|argument: &Argument| argument.key == key)
        {
            return Err(DecodeError::DuplicateArgument);
        }
        arguments.push(Argument::new(key, value));
    }
    let mut message = Message {
        destination,
        version,
        message_type,
        request_id,
        body,
        arguments,
    };
    if body_text == "ERROR" {
        let code_position = message
            .arguments
            .iter()
            .position(|argument| argument.key == "code")
            .ok_or(DecodeError::InvalidBody)?;
        let code = ErrorCode::parse(&message.arguments[code_position].value)
            .ok_or(DecodeError::InvalidBody)?;
        message.arguments.remove(code_position);
        message.body = Body::Error(code);
    }
    if let Err(error) = validate_message(&message) {
        match error {
            ValidationError::UnsupportedCommand => return Ok(message),
            ValidationError::InvalidVersion => return Err(DecodeError::InvalidVersion),
            ValidationError::InvalidRequestId => return Err(DecodeError::InvalidRequestId),
            ValidationError::InvalidArgument => return Err(DecodeError::InvalidArgument),
            ValidationError::InvalidDirection => return Err(DecodeError::InvalidDirection),
            ValidationError::DuplicateArgument => return Err(DecodeError::DuplicateArgument),
            ValidationError::InvalidMessageType => return Err(DecodeError::InvalidMessageType),
            ValidationError::InvalidState
            | ValidationError::TooManyPendingRequests
            | ValidationError::RequestIdInUse
            | ValidationError::RequestIdNotPending => return Err(DecodeError::InvalidBody),
        }
    }
    Ok(message)
}

pub fn encoded_len(message: &Message) -> Result<usize, EncodeError> {
    validate_message(message).map_err(EncodeError::InvalidMessage)?;
    let body = match &message.body {
        Body::Command(command) => command.as_str(),
        Body::Ok => "OK",
        Body::Error(_) => "ERROR",
    };
    let mut length = message.destination.as_str().len()
        + 1
        + decimal_len(message.version as u64)
        + 1
        + message.message_type.as_str().len()
        + 1
        + decimal_len(message.request_id)
        + 1
        + body.len()
        + 1;
    for argument in &message.arguments {
        length += 1 + argument.key.len() + 1 + argument.value.len();
    }
    if matches!(message.body, Body::Error(_)) {
        let Body::Error(code) = message.body else {
            return Err(EncodeError::InvalidMessage(
                ValidationError::InvalidArgument,
            ));
        };
        length += 1 + "code=".len() + code.as_str().len();
    }
    if length > MAX_MESSAGE_LEN {
        return Err(EncodeError::InvalidMessage(
            ValidationError::InvalidArgument,
        ));
    }
    Ok(length)
}

const fn decimal_len(mut value: u64) -> usize {
    let mut length = 1;
    while value >= 10 {
        length += 1;
        value /= 10;
    }
    length
}

pub fn encode_line(message: &Message, output: &mut [u8]) -> Result<usize, EncodeError> {
    let required = encoded_len(message)?;
    if output.len() < required {
        return Err(EncodeError::BufferTooSmall { required });
    }
    let mut cursor = BufferWriter::new(output);
    cursor.write(message.destination.as_str());
    cursor.write(" ");
    cursor.write_u64(message.version as u64);
    cursor.write(" ");
    cursor.write(message.message_type.as_str());
    cursor.write(" ");
    cursor.write_u64(message.request_id);
    cursor.write(" ");
    match &message.body {
        Body::Command(command) => cursor.write(command.as_str()),
        Body::Ok => cursor.write("OK"),
        Body::Error(code) => {
            cursor.write("ERROR code=");
            cursor.write(code.as_str());
        }
    }
    for argument in &message.arguments {
        cursor.write(" ");
        cursor.write(&argument.key);
        cursor.write("=");
        cursor.write(&argument.value);
    }
    cursor.write("\n");
    Ok(required)
}

pub fn encode_to_string(message: &Message) -> Result<String, EncodeError> {
    let length = encoded_len(message)?;
    let mut bytes = alloc::vec![0; length];
    encode_line(message, &mut bytes)?;
    String::from_utf8(bytes)
        .map_err(|_| EncodeError::InvalidMessage(ValidationError::InvalidArgument))
}

struct BufferWriter<'a> {
    output: &'a mut [u8],
    offset: usize,
}

impl<'a> BufferWriter<'a> {
    fn new(output: &'a mut [u8]) -> Self {
        Self { output, offset: 0 }
    }

    fn write(&mut self, value: &str) {
        let end = self.offset + value.len();
        self.output[self.offset..end].copy_from_slice(value.as_bytes());
        self.offset = end;
    }

    fn write_u64(&mut self, mut value: u64) {
        let mut digits = [0_u8; 20];
        let mut cursor = digits.len();
        loop {
            cursor -= 1;
            digits[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        let length = digits.len() - cursor;
        let end = self.offset + length;
        self.output[self.offset..end].copy_from_slice(&digits[cursor..]);
        self.offset = end;
    }
}

#[derive(Debug, Default)]
pub struct ConnectionValidator {
    negotiated: bool,
    pending_request_ids: Vec<u64>,
}

impl ConnectionValidator {
    pub const fn new() -> Self {
        Self {
            negotiated: false,
            pending_request_ids: Vec::new(),
        }
    }

    pub const fn is_negotiated(&self) -> bool {
        self.negotiated
    }

    pub fn accept(&mut self, message: &Message) -> Result<(), ValidationError> {
        validate_message(message)?;
        let command = message.known_command();
        if !self.negotiated
            && !matches!(
                command,
                Some(KnownCommand::ProtocolHello)
                    | Some(KnownCommand::ProtocolWelcome)
                    | Some(KnownCommand::ProtocolSync)
                    | Some(KnownCommand::ProtocolPing)
            )
        {
            return Err(ValidationError::InvalidState);
        }
        if message.message_type == MessageType::Request {
            if self.pending_request_ids.contains(&message.request_id) {
                return Err(ValidationError::RequestIdInUse);
            }
            if self.pending_request_ids.len() >= MAX_PENDING_REQUESTS {
                return Err(ValidationError::TooManyPendingRequests);
            }
            self.pending_request_ids.push(message.request_id);
        }
        if matches!(
            command,
            Some(KnownCommand::ProtocolHello | KnownCommand::ProtocolWelcome)
        ) {
            self.negotiated = true;
        }
        Ok(())
    }

    pub fn complete(&mut self, request_id: u64) -> Result<(), ValidationError> {
        let position = self
            .pending_request_ids
            .iter()
            .position(|pending| *pending == request_id)
            .ok_or(ValidationError::RequestIdNotPending)?;
        self.pending_request_ids.swap_remove(position);
        Ok(())
    }

    pub fn pending_count(&self) -> usize {
        self.pending_request_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_message(command: KnownCommand) -> Message {
        let (destination, message_type, request_id) = match command {
            KnownCommand::ProtocolSync | KnownCommand::ProtocolPing => {
                (Destination::Mboot, MessageType::Request, 1)
            }
            KnownCommand::ProtocolHello => (Destination::Mboot, MessageType::Request, 1),
            KnownCommand::ProtocolWelcome => (Destination::Mochios, MessageType::Response, 1),
            KnownCommand::GuestReady
            | KnownCommand::GuestHeartbeat
            | KnownCommand::GuestStopping
            | KnownCommand::GuestPanic => (Destination::Mboot, MessageType::Event, 0),
            KnownCommand::GuestStatus | KnownCommand::GuestShutdown | KnownCommand::GuestReboot => {
                (Destination::Mochios, MessageType::Request, 1)
            }
            KnownCommand::HostStatus | KnownCommand::HostPoweroff | KnownCommand::HostReboot => {
                (Destination::Mboot, MessageType::Request, 1)
            }
            KnownCommand::DeveloperBegin
            | KnownCommand::DeveloperChunk
            | KnownCommand::DeveloperCompile
            | KnownCommand::DeveloperRead
            | KnownCommand::DeveloperCancel
            | KnownCommand::LinuxLaunch
            | KnownCommand::LinuxStageBegin
            | KnownCommand::LinuxStageChunk
            | KnownCommand::LinuxStageCommit
            | KnownCommand::LinuxStageCancel
            | KnownCommand::LinuxPortalReset
            | KnownCommand::LinuxPortalGrant
            | KnownCommand::LinuxPortalMkdir
            | KnownCommand::LinuxPortalFileBegin
            | KnownCommand::LinuxPortalFileChunk
            | KnownCommand::LinuxPortalFileCommit
            | KnownCommand::LinuxPortalFileCancel
            | KnownCommand::LinuxPortalRelease
            | KnownCommand::LinuxPortalExportBegin
            | KnownCommand::LinuxPortalExportEntry
            | KnownCommand::LinuxPortalExportChunk
            | KnownCommand::LinuxPortalExportEnd
            | KnownCommand::LinuxBundleLaunch
            | KnownCommand::LinuxWindows
            | KnownCommand::LinuxWindowInfo
            | KnownCommand::LinuxFrame
            | KnownCommand::LinuxInput
            | KnownCommand::LinuxConfigure
            | KnownCommand::LinuxClose
            | KnownCommand::WifiStatus
            | KnownCommand::WifiScan
            | KnownCommand::WifiSetEnabled
            | KnownCommand::WifiConnect
            | KnownCommand::WifiDisconnect => (Destination::Mboot, MessageType::Request, 1),
        };
        let arguments = match command {
            KnownCommand::ProtocolHello => alloc::vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
            ],
            KnownCommand::ProtocolWelcome => alloc::vec![
                Argument::new("version", "1"),
                Argument::new("session", "721b95ac"),
                Argument::new("heartbeat_ms", "5000"),
            ],
            KnownCommand::GuestReady => alloc::vec![Argument::new("stage", "kernel")],
            KnownCommand::GuestHeartbeat => {
                alloc::vec![Argument::new("uptime_ms", "10000")]
            }
            KnownCommand::DeveloperBegin => alloc::vec![
                Argument::new("transaction", "7"),
                Argument::new("size", "64"),
            ],
            KnownCommand::DeveloperChunk => alloc::vec![
                Argument::new("transaction", "7"),
                Argument::new("offset", "0"),
                Argument::new("data", "00ff"),
            ],
            KnownCommand::DeveloperCompile | KnownCommand::DeveloperCancel => {
                alloc::vec![Argument::new("transaction", "7")]
            }
            KnownCommand::DeveloperRead => alloc::vec![
                Argument::new("transaction", "7"),
                Argument::new("stream", "output"),
                Argument::new("offset", "0"),
                Argument::new("maximum", "1024"),
            ],
            KnownCommand::LinuxLaunch => alloc::vec![
                Argument::new("application", "xterm"),
                Argument::new("instance", "9"),
            ],
            KnownCommand::LinuxStageBegin => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("bundle", "org.example.editor"),
                Argument::new("size", "4096"),
                Argument::new(
                    "digest",
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                ),
            ],
            KnownCommand::LinuxStageChunk => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("offset", "0"),
                Argument::new("data", "00ff"),
            ],
            KnownCommand::LinuxStageCommit | KnownCommand::LinuxStageCancel => {
                alloc::vec![Argument::new("instance", "9")]
            }
            KnownCommand::LinuxPortalReset
            | KnownCommand::LinuxPortalFileCommit
            | KnownCommand::LinuxPortalFileCancel
            | KnownCommand::LinuxPortalRelease
            | KnownCommand::LinuxPortalExportEnd => {
                alloc::vec![Argument::new("instance", "9")]
            }
            KnownCommand::LinuxPortalGrant => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("grant", "12"),
                Argument::new("access", "read"),
                Argument::new("path", "2f686f6d652f616c696365"),
                Argument::new("mode", "493"),
            ],
            KnownCommand::LinuxPortalMkdir => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("grant", "12"),
                Argument::new("path", "2f686f6d652f616c6963652f446576656c6f70"),
                Argument::new("mode", "493"),
            ],
            KnownCommand::LinuxPortalFileBegin => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("grant", "12"),
                Argument::new("path", "2f686f6d652f616c6963652f446576656c6f702f612e747874"),
                Argument::new("size", "3"),
                Argument::new("mode", "420"),
            ],
            KnownCommand::LinuxPortalFileChunk => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("offset", "0"),
                Argument::new("data", "616263"),
            ],
            KnownCommand::LinuxPortalExportBegin => {
                alloc::vec![Argument::new("instance", "9"), Argument::new("grant", "12"),]
            }
            KnownCommand::LinuxPortalExportEntry => {
                alloc::vec![Argument::new("instance", "9"), Argument::new("index", "0"),]
            }
            KnownCommand::LinuxPortalExportChunk => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("index", "0"),
                Argument::new("offset", "0"),
                Argument::new("maximum", "1024"),
            ],
            KnownCommand::LinuxBundleLaunch => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("bundle", "org.example.editor"),
                Argument::new("entry", "/usr/bin/editor"),
                Argument::new("user", "alice"),
                Argument::new("writable", "/usr/share/editor,/var/lib/editor"),
            ],
            KnownCommand::LinuxWindows => alloc::vec![Argument::new("instance", "9")],
            KnownCommand::LinuxWindowInfo | KnownCommand::LinuxClose => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("window", "12"),
            ],
            KnownCommand::LinuxConfigure => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("window", "12"),
                Argument::new("width", "800"),
                Argument::new("height", "600"),
            ],
            KnownCommand::LinuxFrame => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("window", "12"),
                Argument::new("generation", "3"),
                Argument::new("offset", "0"),
                Argument::new("maximum", "1024"),
            ],
            KnownCommand::LinuxInput => alloc::vec![
                Argument::new("instance", "9"),
                Argument::new("window", "12"),
                Argument::new("kind", "motion"),
                Argument::new("code", "0"),
                Argument::new("value", "0"),
                Argument::new("x", "40"),
                Argument::new("y", "20"),
                Argument::new("modifiers", "0"),
            ],
            KnownCommand::WifiSetEnabled => alloc::vec![Argument::new("enabled", "1")],
            KnownCommand::WifiConnect => alloc::vec![
                Argument::new("ssid", "6d6f6368694f53"),
                Argument::new("security", "secured"),
                Argument::new("credential", "70617373776f7264"),
            ],
            _ => Vec::new(),
        };
        Message::command(destination, message_type, request_id, command, arguments)
    }

    fn round_trip(message: &Message) {
        let encoded = encode_to_string(message).unwrap();
        assert_eq!(decode_line(encoded.as_bytes()).unwrap(), *message);
    }

    #[test]
    fn every_command_round_trips() {
        for command in KnownCommand::ALL {
            round_trip(&valid_message(command));
        }
    }

    #[test]
    fn destinations_and_message_types_round_trip() {
        round_trip(&Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolPing,
            Vec::new(),
        ));
        round_trip(&Message::command(
            Destination::Mochios,
            MessageType::Request,
            2,
            KnownCommand::ProtocolPing,
            Vec::new(),
        ));
        round_trip(&Message::command(
            Destination::Mboot,
            MessageType::Event,
            0,
            KnownCommand::GuestHeartbeat,
            alloc::vec![Argument::new("uptime_ms", "10000")],
        ));
        round_trip(&Message::ok(Destination::Mochios, 3, Vec::new()));
    }

    #[test]
    fn arguments_round_trip() {
        round_trip(&Message::command(
            Destination::Mboot,
            MessageType::Request,
            u64::MAX,
            KnownCommand::ProtocolHello,
            alloc::vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
            ],
        ));
    }

    #[test]
    fn hello_capability_list_round_trips() {
        let message = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolHello,
            alloc::vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
                Argument::new("capabilities", "ready,heartbeat,status"),
            ],
        );
        round_trip(&message);
    }

    #[test]
    fn malformed_lines_are_rejected() {
        assert_eq!(
            decode_line(&alloc::vec![b'a'; 4097]),
            Err(DecodeError::TooLong)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 REQ 1 PROTOCOL.PING \xff\n"),
            Err(DecodeError::InvalidUtf8)
        );
        assert_eq!(
            decode_line(b"@OTHER 1 REQ 1 PROTOCOL.PING\n"),
            Err(DecodeError::UnknownDestination)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 BAD 1 PROTOCOL.PING\n"),
            Err(DecodeError::UnknownMessageType)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 REQ no PROTOCOL.PING\n"),
            Err(DecodeError::InvalidRequestId)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 REQ 0 PROTOCOL.PING\n"),
            Err(DecodeError::InvalidRequestId)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 EVT 1 GUEST.READY stage=kernel\n"),
            Err(DecodeError::InvalidRequestId)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 EVT 0 GUEST.READY broken\n"),
            Err(DecodeError::InvalidArgument)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 EVT 0 GUEST.READY stage=kernel stage=desktop\n"),
            Err(DecodeError::DuplicateArgument)
        );
        assert_eq!(
            decode_line(b"@MBOOT 2 REQ 1 PROTOCOL.PING\n"),
            Err(DecodeError::InvalidVersion)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 REQ 1 PROTOCOL.PING"),
            Err(DecodeError::MissingLineFeed)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 EVT 0 GUEST.READY stage=future\n"),
            Err(DecodeError::InvalidArgument)
        );
    }

    #[test]
    fn unknown_command_is_preserved_as_unsupported() {
        let message = decode_line(b"@MBOOT 1 REQ 5 FUTURE.COMMAND value=yes\n").unwrap();
        assert_eq!(
            message.body,
            Body::Command(Command::Unsupported("FUTURE.COMMAND".into()))
        );
        assert_eq!(
            validate_message(&message),
            Err(ValidationError::UnsupportedCommand)
        );
    }

    #[test]
    fn command_contract_is_enforced() {
        let mut message = valid_message(KnownCommand::GuestReady);
        message.destination = Destination::Mochios;
        assert_eq!(
            validate_message(&message),
            Err(ValidationError::InvalidDirection)
        );
        assert_eq!(
            decode_line(b"@MOCHIOS 1 EVT 0 GUEST.READY stage=kernel\n"),
            Err(DecodeError::InvalidDirection)
        );
        message.destination = Destination::Mboot;
        message.message_type = MessageType::Request;
        message.request_id = 1;
        assert_eq!(
            validate_message(&message),
            Err(ValidationError::InvalidMessageType)
        );
        assert_eq!(
            decode_line(b"@MBOOT 1 REQ 1 GUEST.READY stage=kernel\n"),
            Err(DecodeError::InvalidMessageType)
        );
    }

    #[test]
    fn connection_requires_hello_and_tracks_pending_ids() {
        let mut validator = ConnectionValidator::new();
        assert_eq!(
            validator.accept(&valid_message(KnownCommand::HostStatus)),
            Err(ValidationError::InvalidState)
        );
        let hello = valid_message(KnownCommand::ProtocolHello);
        validator.accept(&hello).unwrap();
        assert_eq!(
            validator.accept(&valid_message(KnownCommand::HostStatus)),
            Err(ValidationError::RequestIdInUse)
        );
        validator.complete(1).unwrap();
        validator
            .accept(&valid_message(KnownCommand::HostStatus))
            .unwrap();

        let mut guest_validator = ConnectionValidator::new();
        guest_validator
            .accept(&valid_message(KnownCommand::ProtocolWelcome))
            .unwrap();
        assert!(guest_validator.is_negotiated());
    }

    #[test]
    fn pending_request_limit_is_enforced() {
        let mut validator = ConnectionValidator::new();
        validator
            .accept(&valid_message(KnownCommand::ProtocolHello))
            .unwrap();
        validator.complete(1).unwrap();
        for request_id in 1..=MAX_PENDING_REQUESTS as u64 {
            let mut message = valid_message(KnownCommand::HostStatus);
            message.request_id = request_id;
            validator.accept(&message).unwrap();
        }
        let mut excess = valid_message(KnownCommand::HostStatus);
        excess.request_id = 33;
        assert_eq!(
            validator.accept(&excess),
            Err(ValidationError::TooManyPendingRequests)
        );
    }

    #[test]
    fn encoder_checks_buffer_and_length() {
        let message = valid_message(KnownCommand::ProtocolPing);
        let required = encoded_len(&message).unwrap();
        let mut short = alloc::vec![0; required - 1];
        assert_eq!(
            encode_line(&message, &mut short),
            Err(EncodeError::BufferTooSmall { required })
        );

        let oversized = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolPing,
            alloc::vec![Argument::new("value", "a".repeat(MAX_MESSAGE_LEN))],
        );
        assert!(matches!(
            encoded_len(&oversized),
            Err(EncodeError::InvalidMessage(_))
        ));

        let base = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolPing,
            alloc::vec![Argument::new("value", "a")],
        );
        let padding = MAX_MESSAGE_LEN - encoded_len(&base).unwrap();
        let boundary = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolPing,
            alloc::vec![Argument::new("value", "a".repeat(padding + 1))],
        );
        assert_eq!(encoded_len(&boundary), Ok(MAX_MESSAGE_LEN));
        round_trip(&boundary);
    }

    #[test]
    fn response_status_round_trips() {
        round_trip(&Message::error(
            Destination::Mochios,
            42,
            ErrorCode::InvalidArgument,
            alloc::vec![Argument::new("field", "timeout_ms")],
        ));
        let text = encode_to_string(&Message::error(
            Destination::Mochios,
            42,
            ErrorCode::InvalidArgument,
            alloc::vec![Argument::new("field", "timeout_ms")],
        ))
        .unwrap();
        assert_eq!(
            text,
            "@MOCHIOS 1 RES 42 ERROR code=invalid_argument field=timeout_ms\n"
        );

        let conflicting = Message::error(
            Destination::Mochios,
            42,
            ErrorCode::InvalidArgument,
            alloc::vec![Argument::new("code", "busy")],
        );
        assert_eq!(
            validate_message(&conflicting),
            Err(ValidationError::DuplicateArgument)
        );
    }

    #[test]
    fn every_error_code_has_wire_value() {
        let codes = [
            ErrorCode::Unsupported,
            ErrorCode::InvalidCommand,
            ErrorCode::InvalidArgument,
            ErrorCode::InvalidState,
            ErrorCode::PermissionDenied,
            ErrorCode::Busy,
            ErrorCode::Timeout,
            ErrorCode::Internal,
        ];
        for code in codes {
            assert_eq!(ErrorCode::parse(code.as_str()), Some(code), "{code:?}");
            round_trip(&Message::error(Destination::Mboot, 1, code, Vec::new()));
        }
    }
}
