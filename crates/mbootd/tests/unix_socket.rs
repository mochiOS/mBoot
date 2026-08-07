use mboot_protocol::{
    decode_line, encode_to_string, Argument, Destination, KnownCommand, Message, MessageType,
};
use mbootd::{run_one, serve_connection, BootStage, ConnectionState, GuestState};
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
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    send(
        &mut stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolSync,
            Vec::new(),
        ),
    );
    let synchronized = read_message(&mut reader);
    assert!(matches!(synchronized.body, mboot_protocol::Body::Ok));
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
    );
    let welcome = read_message(&mut reader);
    assert_eq!(welcome.known_command(), Some(KnownCommand::ProtocolWelcome));
    assert_eq!(welcome.request_id, 2);

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
    drop(reader);
    drop(stream);

    let state = server.join().unwrap();
    assert_eq!(state.connection_state, ConnectionState::Ready);
    assert_eq!(state.boot_stage, Some(BootStage::Desktop));
    assert_eq!(state.system_version.as_deref(), Some("26.0.0"));
    assert_eq!(state.boot_id.as_deref(), Some("01989d34"));
    assert_eq!(state.guest_uptime_ms, Some(10_000));
    assert!(!socket.exists());
}

#[test]
fn reconnect_creates_a_new_session_without_old_guest_state() {
    let (first_server, mut first_client) = UnixStream::pair().unwrap();
    let first = thread::spawn(move || {
        let mut state = GuestState::default();
        serve_connection(first_server, &mut state, 25).unwrap();
        state
    });
    let first_session = negotiate_and_disconnect(&mut first_client, "boot-a", 100);
    let first_state = first.join().unwrap();
    assert_eq!(first_state.boot_id.as_deref(), Some("boot-a"));
    assert_eq!(first_state.guest_uptime_ms, Some(100));

    let (second_server, mut second_client) = UnixStream::pair().unwrap();
    let second = thread::spawn(move || {
        let mut state = GuestState::default();
        serve_connection(second_server, &mut state, 25).unwrap();
        state
    });
    let second_session = negotiate_and_disconnect(&mut second_client, "boot-b", 200);
    let second_state = second.join().unwrap();
    assert_ne!(first_session, second_session);
    assert_eq!(second_state.boot_id.as_deref(), Some("boot-b"));
    assert_eq!(second_state.guest_uptime_ms, Some(200));
    assert_ne!(second_state.boot_id, first_state.boot_id);
}

fn send(stream: &mut UnixStream, message: &Message) {
    let encoded = encode_to_string(message).unwrap();
    stream.write_all(encoded.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn read_message(reader: &mut BufReader<UnixStream>) -> Message {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    decode_line(line.as_bytes()).unwrap()
}

fn negotiate_and_disconnect(stream: &mut UnixStream, boot_id: &str, uptime_ms: u64) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    send(
        stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            1,
            KnownCommand::ProtocolSync,
            Vec::new(),
        ),
    );
    assert!(matches!(
        read_message(&mut reader).body,
        mboot_protocol::Body::Ok
    ));
    send(
        stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Request,
            2,
            KnownCommand::ProtocolHello,
            vec![
                Argument::new("system", "mochios"),
                Argument::new("version", "26.0.0"),
                Argument::new("boot_id", boot_id),
            ],
        ),
    );
    let welcome = read_message(&mut reader);
    let session = welcome.argument("session").unwrap().to_owned();
    for stage in ["kernel", "userspace", "display", "desktop"] {
        send(
            stream,
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
        stream,
        &Message::command(
            Destination::Mboot,
            MessageType::Event,
            0,
            KnownCommand::GuestHeartbeat,
            vec![Argument::new("uptime_ms", uptime_ms.to_string())],
        ),
    );
    stream.shutdown(std::net::Shutdown::Both).unwrap();
    session
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
