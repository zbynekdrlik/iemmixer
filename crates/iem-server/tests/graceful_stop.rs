//! The server stops gracefully (S6 design note §5.5): SIGTERM or SIGINT
//! closes the listener, open requests get up to 5 s, the process exits 0 and
//! the port is free. Unix only: on Windows the request is a Ctrl-Break to the
//! server's own console, proven in the `windows` CI job.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    port: u16,
    dir: tempfile::TempDir,
}

impl Server {
    /// Everything the server logged (stdout and stderr).
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("server.log")).unwrap_or_default()
    }
}

impl Drop for Server {
    /// A failed test still asks its server to stop (without waiting).
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = signal(self.child.id(), "TERM");
        }
    }
}

/// A local port nobody listens on (the probe listener is closed again).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One `GET /api/version` on its own connection; the status line.
fn version_status(port: u16) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET /api/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut text = String::new();
    s.read_to_string(&mut text).ok()?;
    text.lines().next().map(str::to_string)
}

/// The server on a free port with an empty site in a temp directory, once
/// it answers `/api/version`.
fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("iemmixer.toml");
    // A fixed public IP: no detection over the internet at start.
    std::fs::write(&site, "local_public_ip = \"203.0.113.1\"\n").unwrap();
    let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .env("IEMMIXER_CONFIG", &site)
        .env("IEMMIXER_ENGINE_PIPE", dir.path().join("no-engine.sock"))
        .env("PORT", port.to_string())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn iem-server");
    let mut server = Server { child, port, dir };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if version_status(port).as_deref() == Some("HTTP/1.1 200 OK") {
            return server;
        }
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!("the server ended at start ({status}): {}", server.log());
        }
        assert!(
            Instant::now() < deadline,
            "the server never answered: {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Sends the signal `name` (TERM or INT) to `pid`: a request to stop, never
/// a force-end.
fn signal(pid: u32, name: &str) -> std::io::Result<ExitStatus> {
    Command::new("kill")
        .args(["-s", name, &pid.to_string()])
        .status()
}

fn request_stop(server: &Server, name: &str) -> Instant {
    let sent = signal(server.child.id(), name).expect("run kill");
    assert!(sent.success(), "signal {name} not sent");
    Instant::now()
}

/// The exit status within `limit` of `since`, or a panic with the log.
fn exit_within(server: &mut Server, since: Instant, limit: Duration) -> ExitStatus {
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            return status;
        }
        assert!(
            since.elapsed() < limit,
            "still running {limit:?} after the stop request: {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

#[test]
fn sigterm_stops_the_server_with_exit_0_and_frees_the_port() {
    let mut server = start();
    let sent = request_stop(&server, "TERM");
    let status = exit_within(&mut server, sent, Duration::from_secs(6));
    let log = server.log();
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(port_is_free(server.port));
    assert!(log.contains("SIGTERM: stopping"), "{log}");
    assert!(log.contains("HTTP server stopped"), "{log}");
    assert!(log.contains("iem-server stopped"), "{log}");
}

#[test]
fn sigint_stops_it_too() {
    let mut server = start();
    let sent = request_stop(&server, "INT");
    let status = exit_within(&mut server, sent, Duration::from_secs(6));
    let log = server.log();
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(log.contains("SIGINT: stopping"), "{log}");
}

#[test]
fn an_unfinished_request_holds_the_stop_at_most_five_seconds() {
    let mut server = start();
    // A client that never finishes its request (a phone that lost the
    // network mid-request).
    let mut stuck = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stuck
        .write_all(b"GET /api/version HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let sent = request_stop(&server, "TERM");
    // The listener closes at once: no new connection while it drains.
    let refused = loop {
        if TcpStream::connect(("127.0.0.1", server.port)).is_err() {
            break true;
        }
        if sent.elapsed() > Duration::from_secs(2) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(refused, "the listener stayed open: {}", server.log());
    // Up to 5 s for the open request, then the process ends on its own.
    let status = exit_within(&mut server, sent, Duration::from_secs(8));
    assert_eq!(status.code(), Some(0), "{}", server.log());
    assert!(port_is_free(server.port));
    drop(stuck);
}
