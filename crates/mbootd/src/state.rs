use mboot_protocol::{Argument, KnownCommand, Message};
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConnectionState {
    Disconnected,
    Connected,
    Negotiated,
    Booting,
    Ready,
    Unresponsive,
    Stopping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootStage {
    Firmware,
    Kernel,
    Userspace,
    Display,
    Desktop,
}

impl BootStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Firmware => "firmware",
            Self::Kernel => "kernel",
            Self::Userspace => "userspace",
            Self::Display => "display",
            Self::Desktop => "desktop",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "firmware" => Self::Firmware,
            "kernel" => Self::Kernel,
            "userspace" => Self::Userspace,
            "display" => Self::Display,
            "desktop" => Self::Desktop,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct GuestState {
    pub connection_state: ConnectionState,
    pub boot_id: Option<String>,
    pub system_version: Option<String>,
    pub boot_stage: Option<BootStage>,
    pub last_heartbeat: Option<Instant>,
    pub guest_uptime_ms: Option<u64>,
}

impl Default for GuestState {
    fn default() -> Self {
        Self {
            connection_state: ConnectionState::Disconnected,
            boot_id: None,
            system_version: None,
            boot_stage: None,
            last_heartbeat: None,
            guest_uptime_ms: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    MissingArgument(&'static str),
    InvalidArgument(&'static str),
    InvalidState,
}

impl fmt::Display for StateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArgument(name) => write!(formatter, "missing argument: {name}"),
            Self::InvalidArgument(name) => write!(formatter, "invalid argument: {name}"),
            Self::InvalidState => formatter.write_str("invalid connection state"),
        }
    }
}

impl std::error::Error for StateError {}

impl GuestState {
    pub fn connected(&mut self) {
        self.connection_state = ConnectionState::Connected;
        self.boot_id = None;
        self.system_version = None;
        self.boot_stage = None;
        self.last_heartbeat = None;
        self.guest_uptime_ms = None;
    }

    pub fn negotiate(&mut self, message: &Message) -> Result<(), StateError> {
        if self.connection_state != ConnectionState::Connected
            || message.known_command() != Some(KnownCommand::ProtocolHello)
        {
            return Err(StateError::InvalidState);
        }
        let system = required(message, "system")?;
        if system != "mochios" {
            return Err(StateError::InvalidArgument("system"));
        }
        self.system_version = Some(required(message, "version")?.to_owned());
        self.boot_id = Some(required(message, "boot_id")?.to_owned());
        self.connection_state = ConnectionState::Negotiated;
        Ok(())
    }

    pub fn ready(&mut self, message: &Message) -> Result<BootStage, StateError> {
        if !matches!(
            self.connection_state,
            ConnectionState::Negotiated | ConnectionState::Booting | ConnectionState::Ready
        ) {
            return Err(StateError::InvalidState);
        }
        let stage = BootStage::parse(required(message, "stage")?)
            .ok_or(StateError::InvalidArgument("stage"))?;
        let expected = match self.boot_stage {
            None => BootStage::Kernel,
            Some(BootStage::Kernel) => BootStage::Userspace,
            Some(BootStage::Userspace) => BootStage::Display,
            Some(BootStage::Display) => BootStage::Desktop,
            Some(BootStage::Firmware | BootStage::Desktop) => return Err(StateError::InvalidState),
        };
        if stage != expected {
            return Err(StateError::InvalidState);
        }
        self.boot_stage = Some(stage);
        self.connection_state = if stage == BootStage::Desktop {
            ConnectionState::Ready
        } else {
            ConnectionState::Booting
        };
        Ok(stage)
    }

    pub fn heartbeat(&mut self, message: &Message) -> Result<u64, StateError> {
        if !matches!(
            self.connection_state,
            ConnectionState::Negotiated | ConnectionState::Booting | ConnectionState::Ready
        ) {
            return Err(StateError::InvalidState);
        }
        let uptime_ms = required(message, "uptime_ms")?
            .parse::<u64>()
            .map_err(|_| StateError::InvalidArgument("uptime_ms"))?;
        self.last_heartbeat = Some(Instant::now());
        self.guest_uptime_ms = Some(uptime_ms);
        Ok(uptime_ms)
    }

    pub fn stopping(&mut self) -> Result<(), StateError> {
        if matches!(
            self.connection_state,
            ConnectionState::Disconnected | ConnectionState::Connected
        ) {
            return Err(StateError::InvalidState);
        }
        self.connection_state = ConnectionState::Stopping;
        Ok(())
    }

    pub fn check_heartbeat_timeout(&mut self, timeout: Duration, now: Instant) -> bool {
        let timed_out = self
            .last_heartbeat
            .is_some_and(|last| now.saturating_duration_since(last) >= timeout);
        if timed_out && self.connection_state != ConnectionState::Stopping {
            self.connection_state = ConnectionState::Unresponsive;
        }
        timed_out
    }

    pub fn status_arguments(&self) -> Vec<Argument> {
        let mut arguments = Vec::new();
        arguments.push(Argument::new(
            "state",
            match self.connection_state {
                ConnectionState::Disconnected => "disconnected",
                ConnectionState::Connected => "connected",
                ConnectionState::Negotiated => "negotiated",
                ConnectionState::Booting => "booting",
                ConnectionState::Ready => "ready",
                ConnectionState::Unresponsive => "unresponsive",
                ConnectionState::Stopping => "stopping",
            },
        ));
        if let Some(stage) = self.boot_stage {
            arguments.push(Argument::new("stage", stage.as_str()));
        }
        if let Some(uptime_ms) = self.guest_uptime_ms {
            arguments.push(Argument::new("uptime_ms", uptime_ms.to_string()));
        }
        arguments
    }
}

fn required<'a>(message: &'a Message, key: &'static str) -> Result<&'a str, StateError> {
    message
        .argument(key)
        .ok_or(StateError::MissingArgument(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mboot_protocol::{Destination, MessageType};

    fn event(command: KnownCommand, arguments: Vec<Argument>) -> Message {
        Message::command(
            Destination::Mboot,
            MessageType::Event,
            0,
            command,
            arguments,
        )
    }

    fn negotiated_state() -> GuestState {
        let mut state = GuestState::default();
        state.connected();
        let hello = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolHello,
            vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
            ],
        );
        state.negotiate(&hello).unwrap();
        state
    }

    #[test]
    fn ready_stages_transition_to_ready() {
        let mut state = negotiated_state();
        for stage in ["kernel", "userspace", "display"] {
            let message = event(
                KnownCommand::GuestReady,
                vec![Argument::new("stage", stage)],
            );
            state.ready(&message).unwrap();
            assert_eq!(state.connection_state, ConnectionState::Booting);
        }
        let desktop = event(
            KnownCommand::GuestReady,
            vec![Argument::new("stage", "desktop")],
        );
        state.ready(&desktop).unwrap();
        assert_eq!(state.connection_state, ConnectionState::Ready);
        assert_eq!(state.boot_stage, Some(BootStage::Desktop));
    }

    #[test]
    fn unknown_ready_stage_is_rejected() {
        let mut state = negotiated_state();
        let message = event(
            KnownCommand::GuestReady,
            vec![Argument::new("stage", "future")],
        );
        assert_eq!(
            state.ready(&message),
            Err(StateError::InvalidArgument("stage"))
        );
    }

    #[test]
    fn ready_stages_cannot_skip_or_move_backwards() {
        let mut state = negotiated_state();
        let display = event(
            KnownCommand::GuestReady,
            vec![Argument::new("stage", "display")],
        );
        assert_eq!(state.ready(&display), Err(StateError::InvalidState));
        let kernel = event(
            KnownCommand::GuestReady,
            vec![Argument::new("stage", "kernel")],
        );
        assert_eq!(state.ready(&kernel), Ok(BootStage::Kernel));
        assert_eq!(state.ready(&kernel), Err(StateError::InvalidState));
    }

    #[test]
    fn heartbeat_updates_state_and_can_time_out() {
        let mut state = negotiated_state();
        for (name, expected) in [
            ("kernel", BootStage::Kernel),
            ("userspace", BootStage::Userspace),
            ("display", BootStage::Display),
            ("desktop", BootStage::Desktop),
        ] {
            let ready = event(KnownCommand::GuestReady, vec![Argument::new("stage", name)]);
            assert_eq!(state.ready(&ready), Ok(expected));
        }
        assert_eq!(state.connection_state, ConnectionState::Ready);
        let message = event(
            KnownCommand::GuestHeartbeat,
            vec![Argument::new("uptime_ms", "10000")],
        );
        state.heartbeat(&message).unwrap();
        let heartbeat = state.last_heartbeat.unwrap();
        assert_eq!(state.guest_uptime_ms, Some(10_000));
        assert!(state
            .check_heartbeat_timeout(Duration::from_secs(5), heartbeat + Duration::from_secs(5)));
        assert_eq!(state.connection_state, ConnectionState::Unresponsive);
    }

    #[test]
    fn hello_ignores_unknown_arguments() {
        let mut state = GuestState::default();
        state.connected();
        let hello = Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolHello,
            vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
                Argument::new("future", "supported"),
            ],
        );
        state.negotiate(&hello).unwrap();
        assert_eq!(state.connection_state, ConnectionState::Negotiated);
    }
}
