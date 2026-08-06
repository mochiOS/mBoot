use mboot_protocol::{
    decode_line, encode_to_string, Argument, Destination, KnownCommand, Message, MessageType,
};
use mbootd::{run_one, BootStage, ConnectionState, GuestState};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn mock_guest_negotiates_over_unix_socket() {
    let socket = temporary_socket();
    let server_socket = socket.clone();
    let server = thread::spawn(move || {
        let mut state = GuestState::default();
        run_one(&server_socket, &mut state).unwrap();
        state
    });

    let mut stream = connect_with_retry(&socket);
    send(
        &mut stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolHello,
            vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", "01989d34"),
            ],
        ),
    );
    let mut welcome = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut welcome)
        .unwrap();
    let welcome = decode_line(welcome.as_bytes()).unwrap();
    assert_eq!(welcome.known_command(), Some(KnownCommand::ProtocolWelcome));
    assert_eq!(welcome.request_id, 1);

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
        );
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
    );
    drop(stream);

    let state = server.join().unwrap();
    assert_eq!(state.connection_state, ConnectionState::Ready);
    assert_eq!(state.boot_stage, Some(BootStage::Desktop));
    assert_eq!(state.system_version.as_deref(), Some("26.0.0"));
    assert_eq!(state.boot_id.as_deref(), Some("01989d34"));
    assert_eq!(state.guest_uptime_ms, Some(10_000));
    let _ = fs::remove_file(socket);
}

fn send(stream: &mut UnixStream, message: &Message) {
    let encoded = encode_to_string(message).unwrap();
    stream.write_all(encoded.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn connect_with_retry(path: &PathBuf) -> UnixStream {
    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(path) {
            return stream;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("mbootd did not create {}", path.display());
}

fn temporary_socket() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::current_dir().unwrap().join("target/test-sockets");
    fs::create_dir_all(&directory).unwrap();
    directory.join(format!("mbootd-{nonce}-{}.sock", std::process::id()))
}
