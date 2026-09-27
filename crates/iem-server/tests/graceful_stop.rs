//! The server stops gracefully (S6 design note §5.5): SIGTERM or SIGINT
//! closes the HTTP and the HTTPS listener at once, open requests get up to
//! 5 s, the process exits 0 and the ports are free.
//!
//! Unix only. The Windows variant (the server started detached with its own
//! console, the PC's exact shape, and stopped with a Ctrl-Break to that
//! console through `iem_win::console::ctrl_break`) is still pending: it needs
//! `iem-win` (S6 plan Task 2) and joins the `windows` CI job in Task 12.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
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

/// A local port nobody listens on (the probe listener is closed again) and
/// that is none of `taken`.
fn free_port_except(taken: &[u16]) -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !taken.contains(&port) {
            return port;
        }
    }
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
    launch(tempfile::tempdir().unwrap(), "", &[], &[])
}

/// The server in `dir` with the site lines `extra` and the environment
/// `envs`, on a free HTTP port that is none of `taken`, once it answers
/// `/api/version`.
fn launch(dir: tempfile::TempDir, extra: &str, envs: &[(&str, PathBuf)], taken: &[u16]) -> Server {
    let site = dir.path().join("iemmixer.toml");
    // A fixed public IP: no detection over the internet at start.
    std::fs::write(&site, format!("local_public_ip = \"203.0.113.1\"\n{extra}")).unwrap();
    let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
    let port = free_port_except(taken);
    let mut command = Command::new(env!("CARGO_BIN_EXE_iem-server"));
    command
        .env("IEMMIXER_CONFIG", &site)
        .env("IEMMIXER_ENGINE_PIPE", dir.path().join("no-engine.sock"))
        .env("PORT", port.to_string())
        .env("NO_COLOR", "1");
    for (name, value) in envs {
        command.env(name, value);
    }
    let child = command
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

/// A client that never finishes its request (a phone that lost the network
/// mid-request): it holds the HTTP drain for its full 5 s.
fn unfinished_request(port: u16) -> TcpStream {
    let mut stuck = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stuck
        .write_all(b"GET /api/version HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    stuck
}

/// Whether a new connection to `port` is refused within 2 s of `since`.
fn refused_within_2s(port: u16, since: Instant) -> bool {
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        if since.elapsed() > Duration::from_secs(2) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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
    let stuck = unfinished_request(server.port);
    let sent = request_stop(&server, "TERM");
    // The listener closes at once: no new connection while it drains.
    assert!(
        refused_within_2s(server.port, sent),
        "the listener stayed open: {}",
        server.log()
    );
    // Up to 5 s for the open request, then the process ends on its own.
    let status = exit_within(&mut server, sent, Duration::from_secs(8));
    assert_eq!(status.code(), Some(0), "{}", server.log());
    assert!(port_is_free(server.port));
    drop(stuck);
}

/// A self-signed certificate for localhost, `cert.pem` and `key.pem` in
/// `dir`, made with the `openssl` utility.
#[cfg(feature = "tls")]
fn cert_in(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let out = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "ec", "-pkeyopt"])
        .args(["ec_paramgen_curve:prime256v1", "-nodes", "-days", "1"])
        .args(["-subj", "/CN=localhost", "-keyout"])
        .arg(dir.join("key.pem"))
        .arg("-out")
        .arg(dir.join("cert.pem"))
        .output()
        .expect("run openssl");
    assert!(
        out.status.success(),
        "openssl made no test certificate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The server with TLS on (a self-signed certificate) and its HTTPS port,
/// once both ports listen.
#[cfg(feature = "tls")]
fn start_https() -> (Server, u16) {
    let dir = tempfile::tempdir().unwrap();
    cert_in(dir.path());
    let https_port = free_port_except(&[]);
    let server = launch(
        dir,
        &format!("tls = true\nhttps_port = {https_port}\n"),
        &[],
        &[https_port],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    // A probe connection, closed at once (it never starts its handshake).
    while TcpStream::connect(("127.0.0.1", https_port)).is_err() {
        assert!(
            Instant::now() < deadline,
            "HTTPS never listened: {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    (server, https_port)
}

/// The certificate lives next to the site file, in the server's config
/// directory (S6 design note §6: `iem-migrate band` writes it into the band
/// directory, the folder of the guard's `server_config`), whatever the
/// platform's config directory holds.
#[cfg(feature = "tls")]
#[test]
fn the_certificate_next_to_the_site_serves_https() {
    let dir = tempfile::tempdir().unwrap();
    cert_in(dir.path());
    let elsewhere = dir.path().join("platform-config");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let https_port = free_port_except(&[]);
    let server = launch(
        dir,
        &format!("tls = true\nhttps_port = {https_port}\n"),
        &[("XDG_CONFIG_HOME", elsewhere)],
        &[https_port],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", https_port)).is_err() {
        assert!(
            Instant::now() < deadline,
            "HTTPS never listened: {}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !server.log().contains("cert files not found"),
        "{}",
        server.log()
    );
}

/// The HTTPS port (P9: the band's 443) closes with the HTTP one, while an
/// open HTTP request still drains.
#[cfg(feature = "tls")]
#[test]
fn the_https_listener_closes_with_the_http_one() {
    let (mut server, https_port) = start_https();
    let stuck = unfinished_request(server.port);
    let sent = request_stop(&server, "TERM");
    assert!(
        refused_within_2s(https_port, sent),
        "the HTTPS listener stayed open during the drain: {}",
        server.log()
    );
    let status = exit_within(&mut server, sent, Duration::from_secs(8));
    let log = server.log();
    assert_eq!(status.code(), Some(0), "{log}");
    assert!(port_is_free(https_port) && port_is_free(server.port));
    assert!(log.contains("HTTPS server stopped"), "{log}");
    drop(stuck);
}

/// An open HTTPS connection (a phone mid-handshake) gets the drain too: the
/// process waits for it up to 5 s instead of cutting it after 1 s.
#[cfg(feature = "tls")]
#[test]
fn an_open_https_connection_is_drained_not_cut() {
    let (mut server, https_port) = start_https();
    let stuck = TcpStream::connect(("127.0.0.1", https_port)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let sent = request_stop(&server, "TERM");
    assert!(
        refused_within_2s(https_port, sent),
        "the HTTPS listener stayed open: {}",
        server.log()
    );
    let status = exit_within(&mut server, sent, Duration::from_secs(8));
    let took = sent.elapsed();
    assert_eq!(status.code(), Some(0), "{}", server.log());
    assert!(
        took >= Duration::from_secs(4),
        "stopped after {took:?}: the open HTTPS connection got no drain: {}",
        server.log()
    );
    assert!(port_is_free(https_port));
    drop(stuck);
}
