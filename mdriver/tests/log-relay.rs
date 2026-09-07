use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mdriver-log-test-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("kmsg"), "6,40,1234,-;probe-enter i915\n3,41,1235,-;i915-error I915 MMIO rc=-5\n").unwrap();
        fs::write(path.join("gpu_probe"), "GPU PROBE REPORT 0000:00:02.0\nBAR0 start=a0000000 size=1000000\n").unwrap();
        fs::write(path.join("start"), "").unwrap();
        Self(path)
    }
    fn spawn(&self, port: u16, once: bool) -> Sender {
        let mut cmd = Command::new(std::env::var_os("MDRIVER_LOG_BINARY").unwrap());
        cmd.args(["--server", "127.0.0.1", "--port", &port.to_string()])
            .arg("--log").arg(self.0.join("kmsg"))
            .arg("--reports").arg(self.0.join("gpu_probe"))
            .arg("--probe-start").arg(self.0.join("start"));
        if once { cmd.arg("--once"); }
        Sender(cmd.spawn().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
struct Sender(Child);
impl Drop for Sender {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn accept(listener: &TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut stream = loop {
        if let Ok((stream, _)) = listener.accept() { break stream; }
        assert!(std::time::Instant::now() < deadline, "sender did not connect");
        std::thread::sleep(Duration::from_millis(10));
    };
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut hello = [0; 17];
    // Keep the wire length derived from the actual protocol string.
    let mut hello = &mut hello[..b"MDEV-LOG/1 HELLO\n".len()];
    stream.read_exact(&mut hello).unwrap();
    assert_eq!(hello, b"MDEV-LOG/1 HELLO\n");
    stream
}

#[test]
fn acknowledged_receiver_gets_probe_context_and_kernel_history() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut sender = fixture.spawn(listener.local_addr().unwrap().port(), true);
    let mut stream = accept(&listener);
    assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"");
    // Exercise fragmented TCP acknowledgement reads.
    stream.write_all(b"MDEV-LOG/1 ").unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"");
    stream.write_all(b"READY\n").unwrap();
    let mut log = String::new();
    stream.read_to_string(&mut log).unwrap();
    assert!(sender.0.wait().unwrap().success());
    assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"1\n");
    assert!(log.contains("GPU PROBE REPORT 0000:00:02.0"));
    assert!(log.contains("BAR0 start=a0000000"));
    assert!(log.contains("i915-error I915 MMIO rc=-5"));
}

#[test]
fn wrong_acknowledgement_does_not_start_gpu() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut sender = fixture.spawn(listener.local_addr().unwrap().port(), true);
    let mut stream = accept(&listener);
    stream.write_all(b"MDEV-LOG/1 WRONG\n").unwrap();
    assert!(!sender.0.wait().unwrap().success());
    assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"");
}

#[test]
fn reconnect_replays_history_without_releasing_probe_twice() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _sender = fixture.spawn(listener.local_addr().unwrap().port(), false);
    for iteration in 0..2 {
        let mut stream = accept(&listener);
        stream.write_all(b"MDEV-LOG/1 READY\n").unwrap();
        let mut log = Vec::new();
        let mut buf = [0; 1024];
        while !String::from_utf8_lossy(&log).contains("I915 MMIO rc=-5") {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            log.extend_from_slice(&buf[..n]);
        }
        if iteration == 0 {
            assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"1\n");
            fs::write(fixture.0.join("start"), "already-started").unwrap();
        } else {
            assert_eq!(fs::read(fixture.0.join("start")).unwrap(), b"already-started");
        }
    }
}
