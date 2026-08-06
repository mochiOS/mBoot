use mboot_protocol::{
    decode_line, encode_to_string, Argument, Destination, KnownCommand, Message, MessageType,
};
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

const DEFAULT_SOCKET_PATH: &str = "/run/mboot/mochios-control.sock";

fn main() -> ExitCode {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_owned());
    match run(Path::new(&path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mock-mochios-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: &Path) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    send(
        &mut stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolSync,
            Vec::new(),
        ),
    )?;
    let mut sync_response = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut sync_response)?;
    let sync_response = decode_line(sync_response.as_bytes()).map_err(invalid_data)?;
    if sync_response.destination != Destination::Mochios
        || sync_response.message_type != MessageType::Response
        || sync_response.request_id != 1
        || !matches!(sync_response.body, mboot_protocol::Body::Ok)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid PROTOCOL.SYNC response",
        ));
    }
    send(
        &mut stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            2,
            KnownCommand::ProtocolHello,
            vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
            ],
        ),
    )?;

    let mut response = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut response)?;
    let welcome = decode_line(response.as_bytes()).map_err(invalid_data)?;
    if welcome.destination != Destination::Mochios
        || welcome.message_type != MessageType::Response
        || welcome.request_id != 2
        || welcome.known_command() != Some(KnownCommand::ProtocolWelcome)
        || welcome.argument("version") != Some("1")
        || welcome.argument("session").is_none()
        || welcome.argument("heartbeat_ms").is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid PROTOCOL.WELCOME",
        ));
    }
    println!("protocol welcome verified");

    for stage in ["kernel", "userspace", "display", "desktop"] {
        send(
            &mut stream,
            &Message::command(
                Destination::Mboot,
                MessageType::Event,
                0,
                KnownCommand::GuestReady,
                vec![Argument::new("stage", stage)],
            ),
        )?;
    }
    send(
        &mut stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Event,
            0,
            KnownCommand::GuestHeartbeat,
            vec![Argument::new("uptime_ms", "10000")],
        ),
    )
}

fn send(stream: &mut UnixStream, message: &Message) -> io::Result<()> {
    let encoded = encode_to_string(message).map_err(invalid_data)?;
    stream.write_all(encoded.as_bytes())?;
    stream.flush()
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
