//! The soak client against a fake server (S7 plan Task 6): the server's
//! login, `/api/site`, the mixer socket and the listen socket as the real
//! server serves them, on `127.0.0.1:0`, one thread per connection. Every
//! run is bounded: it runs on its own thread and the test waits for it with
//! `recv_timeout`, so a run that never ends fails here instead of hanging.
//! Timing is asserted only from below (a wait that must have happened) or
//! against the run's own measured length, never as a fixed sleep. A run
//! that must end early is a 30 s run waited for 5 or 6 s.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use iem_soakclient::net::{Limits, run};
use iem_soakclient::{Args, PIN_ENV, Reason, Summary, write_summary};
use serde_json::json;
use tungstenite::{Message, WebSocket};

/// The engineer's PIN the fake accepts, and the token it issues.
const PIN: &str = "1234";
const TOKEN: &str = "T";
/// The two sockets, as the client must ask for them.
const MIXER: &str = "/ws/member9?token=T&proto=2";
const LISTEN: &str = "/ws/audio?token=T";
const START: &str = r#"{"cmd":"ListenStart","member_id":"member9"}"#;
const STOP: &str = r#"{"cmd":"ListenStop"}"#;
/// The fake's reads and writes wait at most this long.
const WAIT: Duration = Duration::from_secs(5);
/// One listen frame or meter frame every 20 ms.
const FRAME: Duration = Duration::from_millis(20);
/// The mixer socket's silence after `State`: longer than the client's read
/// timeout, so the client's reads time out on an open socket.
const SILENCE: Duration = Duration::from_millis(300);

/// What the fake answers.
#[derive(Clone)]
struct Script {
    /// `/api/site`'s `lan_url` (`null` when `None`).
    lan_url: Option<String>,
    /// The login's `engineer`.
    engineer: bool,
    /// The listen socket sends this many frames and one frame Opus refuses,
    /// then it is dropped without a Close.
    drop_listen_after: Option<usize>,
    /// With `drop_listen_after`: the listen socket sends a Close instead of
    /// being dropped, and reads the client's answer.
    close_not_drop: bool,
    /// Every upgrade is refused before its handshake.
    refuse_upgrades: bool,
    /// The mixer socket sends `Hello` and `State`, then nothing, and stays
    /// open (it still reads the client).
    stall_mixer: bool,
    /// Once the listen socket hears the client's `ListenStop`, it reads
    /// nothing more and holds the connection for `WAIT`: the client's Close
    /// is never answered.
    deaf_after_stop: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            lan_url: None,
            engineer: true,
            drop_listen_after: None,
            close_not_drop: false,
            refuse_upgrades: false,
            stall_mixer: false,
            deaf_after_stop: false,
        }
    }
}

struct Fake {
    addr: SocketAddr,
    /// The target (path and query) of every request, HTTP and upgrade alike.
    seen: mpsc::Receiver<String>,
    /// What the client sent on its sockets after `ListenStart`, as
    /// "<socket> <text>".
    heard: mpsc::Receiver<String>,
    /// The sockets ("mixer", "listen") the client ended with a Close.
    closed: mpsc::Receiver<String>,
}

/// Where the fake reports.
struct Report {
    seen: mpsc::Sender<String>,
    heard: mpsc::Sender<String>,
    closed: mpsc::Sender<String>,
}

impl Fake {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen) = mpsc::channel();
        let (heard_tx, heard) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let report = Report {
            seen: seen_tx,
            heard: heard_tx,
            closed: closed_tx,
        };
        let shared = Arc::new((script, report));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = Arc::clone(&shared);
                thread::spawn(move || connection(stream, &shared.0, &shared.1));
            }
        });
        Self {
            addr,
            seen,
            heard,
            closed,
        }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The targets requested so far.
    fn seen(&self) -> Vec<String> {
        self.seen.try_iter().collect()
    }

    /// What the client sent on its sockets, up to its `ListenStop` (which
    /// comes as the run ends), each wait at most 2 s.
    fn heard(&self) -> Vec<String> {
        let mut heard = Vec::new();
        while let Ok(text) = self.heard.recv_timeout(Duration::from_secs(2)) {
            let stop = text == format!("listen {STOP}");
            heard.push(text);
            if stop {
                break;
            }
        }
        heard
    }

    /// The sockets the client ended with a Close, sorted: `n` of them
    /// waited for (each at most 2 s), and any more already there.
    fn closed(&self, n: usize) -> Vec<String> {
        let wait = || self.closed.recv_timeout(Duration::from_secs(2)).ok();
        let mut closed: Vec<String> = (0..n).map_while(|_| wait()).collect();
        closed.extend(self.closed.try_iter());
        closed.sort();
        closed
    }
}

fn connection(stream: TcpStream, script: &Script, report: &Report) {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let Some(head) = peek_head(&stream) else {
        return;
    };
    let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
    let _ = report.seen.send(target.clone());
    if head.to_ascii_lowercase().contains("upgrade: websocket") {
        upgrade(stream, &target, script, report);
    } else {
        http(stream, head.len(), &target, script);
    }
}

/// The request head through its blank line, peeked: an upgrade's handshake
/// reads it again.
fn peek_head(stream: &TcpStream) -> Option<String> {
    let mut buf = [0u8; 4096];
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        let n = stream.peek(&mut buf).ok().filter(|n| *n > 0)?;
        let text = String::from_utf8_lossy(&buf[..n]);
        if let Some(end) = text.find("\r\n\r\n") {
            return Some(text[..end + 4].to_owned());
        }
        thread::sleep(Duration::from_millis(1));
    }
    None
}

fn http(mut stream: TcpStream, head_len: usize, target: &str, script: &Script) {
    let mut head = vec![0u8; head_len];
    stream.read_exact(&mut head).unwrap();
    let length = String::from_utf8_lossy(&head)
        .lines()
        .find_map(|line| {
            let line = line.to_ascii_lowercase();
            line.strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    let (status, reply) = match target {
        "/api/site" => (
            "200 OK",
            json!({"lan_url": script.lan_url, "public_host": "mixer.example.org"}),
        ),
        "/api/auth" => {
            let login: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            if login == json!({"member": "engineer", "pin": PIN}) {
                let token = json!({"token": TOKEN, "member": "engineer",
                    "engineer": script.engineer, "expires_in": 604_800});
                ("200 OK", token)
            } else {
                let refused = json!({"code": "INVALID_PIN", "message": "Invalid PIN"});
                ("401 Unauthorized", refused)
            }
        }
        _ => ("404 Not Found", json!({})),
    };
    let reply = reply.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{reply}",
        reply.len()
    );
}

/// The socket at `target` (another path or token, or a `refuse_upgrades`
/// script: refused before the handshake).
fn upgrade(stream: TcpStream, target: &str, script: &Script, report: &Report) {
    let listen = match target {
        MIXER => false,
        LISTEN => true,
        _ => return,
    };
    if script.refuse_upgrades {
        return;
    }
    let Ok(mut ws) = tungstenite::accept(stream) else {
        return;
    };
    if listen {
        listen_stream(&mut ws, script, report);
    } else {
        mixer_stream(&mut ws, script.stall_mixer, report);
    }
}

type Ws = WebSocket<TcpStream>;

/// Reads what the client sends, for at most `wait` or until it leaves
/// (false); each text goes to `heard` as "<socket> <text>", a Close to
/// `closed` (the next read sends the answering Close). `deaf`: after the
/// client's `ListenStop` nothing more is read, the connection is held for
/// `WAIT`, and the client's Close is never answered.
fn hear(ws: &mut Ws, socket: &str, report: &Report, wait: Duration, deaf: bool) -> bool {
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        ws.get_mut().set_read_timeout(Some(left)).unwrap();
        match ws.read() {
            Ok(Message::Text(text)) => {
                let _ = report.heard.send(format!("{socket} {text}"));
                if deaf && text.as_str() == STOP {
                    thread::sleep(WAIT);
                    return false;
                }
            }
            Ok(Message::Close(_)) => {
                let _ = report.closed.send(socket.to_owned());
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return true;
            }
            Err(_) => return false,
        }
    }
}

/// Sends `next()` every 20 ms on a fixed schedule, hearing the client in
/// between, until `next` gives nothing (the socket is then dropped at once,
/// without a Close) or the client leaves (its last words are still read).
fn stream(
    ws: &mut Ws,
    socket: &str,
    report: &Report,
    deaf: bool,
    mut next: impl FnMut() -> Option<Message>,
) {
    let mut at = Instant::now();
    while let Some(message) = next() {
        if ws.send(message).is_err() {
            hear(ws, socket, report, Duration::from_millis(200), deaf);
            return;
        }
        at += FRAME;
        let left = at.saturating_duration_since(Instant::now());
        if !hear(ws, socket, report, left, deaf) {
            return;
        }
    }
}

/// `Hello`, `State`, a silence, then `Meters` every 20 ms until the client
/// is gone. A stalled socket sends nothing more and stays open, reading,
/// until the client leaves (or 5 s).
fn mixer_stream(ws: &mut Ws, stall: bool, report: &Report) {
    let hello = r#"{"event":"Hello","data":{"proto":2,"build":"local","min_client_proto":2}}"#;
    let state = r#"{"event":"State","data":{"channels":[],"connected":true}}"#;
    for text in [hello, state] {
        if ws.send(Message::text(text)).is_err() {
            return;
        }
    }
    let silence = if stall { WAIT } else { SILENCE };
    if !hear(ws, "mixer", report, silence, false) || stall {
        return;
    }
    let meters = r#"{"event":"Meters","data":{"meters":{"mic1":[0.1,0.1]}}}"#;
    stream(ws, "mixer", report, false, || Some(Message::text(meters)));
}

/// Waits for `ListenStart` on member9, answers `listening`, then sends one
/// Opus packet of silence every 20 ms until the client is gone (a
/// `drop_listen_after` script cuts it short: a drop, or a Close).
fn listen_stream(ws: &mut Ws, script: &Script, report: &Report) {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) if text.as_str() == START => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let listening = r#"{"event":"AudioStatus","data":{"status":"listening","target":"member9"}}"#;
    if ws.send(Message::text(listening)).is_err() {
        return;
    }
    let mut encoder =
        opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
    let cut = script.drop_listen_after;
    let mut sent = 0;
    stream(ws, "listen", report, script.deaf_after_stop, || {
        let frame = match cut {
            // Then the socket is dropped.
            Some(cut) if sent > cut => return None,
            // A frame Opus refuses (63 frames of 20 ms in one packet).
            Some(cut) if sent == cut => Message::binary(vec![0xffu8; 3]),
            _ => Message::binary(encoder.encode_vec_float(&[0.0; 1920], 4000).unwrap()),
        };
        sent += 1;
        Some(frame)
    });
    if script.close_not_drop {
        // The client's answering Close goes to `closed`.
        let _ = ws.close(None);
        hear(ws, "listen", report, WAIT, false);
    }
}

/// The arguments of a run of `seconds` on member9 at `base`.
fn args(base: &str, direct: bool, seconds: u64) -> Args {
    Args {
        base: base.to_owned(),
        direct,
        member: "member9".to_owned(),
        seconds,
        out: PathBuf::from("unused.json"),
        cpu_sets: Vec::new(),
    }
}

/// The tests' bounds: a summary every 500 ms, reads wait 100 ms, a socket
/// silent for 1 s ends the run (the mixer's silence after `State` is
/// 300 ms), and a socket waits at most 1 s for the fake's Close.
fn limits() -> Limits {
    Limits {
        write_every: Duration::from_millis(500),
        read_timeout: Duration::from_millis(100),
        idle: Duration::from_secs(1),
        close_wait: Duration::from_secs(1),
    }
}

struct Ran {
    summary: Summary,
    /// Every summary `run` handed to its writer, in order, with when it came
    /// (since the run began).
    written: Vec<(Duration, Summary)>,
    took: Duration,
}

/// `run` on its own thread, waited for at most `bound`; each summary is also
/// written to `out` when given.
fn run_within(args: Args, pin: &'static str, bound: Duration, out: Option<PathBuf>) -> Ran {
    let (tx, rx) = mpsc::channel();
    let began = Instant::now();
    thread::spawn(move || {
        let mut written = Vec::new();
        let summary = run(&args, pin, &limits(), &mut |s: &Summary| {
            if let Some(out) = &out {
                write_summary(out, s).unwrap();
            }
            written.push((began.elapsed(), s.clone()));
        });
        let _ = tx.send((summary, written));
    });
    let (summary, written) = rx
        .recv_timeout(bound)
        .expect("the run ends within its bound");
    Ran {
        summary,
        written,
        took: began.elapsed(),
    }
}

/// How often `target` was requested.
fn times(seen: &[String], target: &str) -> usize {
    seen.iter().filter(|t| *t == target).count()
}

/// The binary with `argv` and the PIN in its environment when given, waited
/// for at most 8 s.
fn exe(argv: Vec<String>, pin: Option<&'static str>) -> Output {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iem-soakclient"));
        cmd.args(argv).env_remove(PIN_ENV);
        if let Some(pin) = pin {
            cmd.env(PIN_ENV, pin);
        }
        let _ = tx.send(cmd.output());
    });
    let out = rx.recv_timeout(Duration::from_secs(8));
    out.expect("the binary ends within 8 s").unwrap()
}

#[test]
fn a_whole_run_counts_frames_and_meters_and_writes_a_summary_every_period() {
    let fake = Fake::start(Script::default());
    let ran = run_within(
        args(&fake.origin(), true, 3),
        PIN,
        Duration::from_secs(8),
        None,
    );
    let s = &ran.summary;
    assert!(s.complete, "{s:?}");
    assert_eq!(s.error, None);
    assert!(s.seconds >= 3.0, "{s:?}");
    assert_eq!((s.reconnects, s.decode_errors, s.no_source), (0, 0, 0));
    assert!(s.frames >= 60, "{s:?}");
    assert!(s.meter_frames >= 60, "{s:?}");
    assert!(s.first_frame_ms.is_some(), "{s:?}");
    // A summary every 500 ms (never sooner), the last one the end's.
    let (at, written): (Vec<Duration>, Vec<Summary>) = ran.written.into_iter().unzip();
    assert_eq!(written.last(), Some(s));
    let every = limits().write_every;
    let periodic = &at[..at.len() - 1];
    assert!(periodic.len() >= 2, "{at:?}");
    assert!(periodic[0] >= every, "{at:?}");
    assert!(periodic.windows(2).all(|w| w[1] - w[0] >= every), "{at:?}");
    assert!(written.iter().rev().skip(1).all(|w| !w.complete));
    // One login, each socket once, and both ended with a Close.
    let seen = fake.seen();
    let counts = [
        times(&seen, "/api/auth"),
        times(&seen, MIXER),
        times(&seen, LISTEN),
    ];
    assert_eq!(counts, [1, 1, 1], "{seen:?}");
    assert_eq!(times(&seen, "/api/site"), 0, "--direct reads no /api/site");
    assert_eq!(fake.closed(2), ["listen", "mixer"]);
}

#[test]
fn the_sockets_go_to_the_lan_url_the_server_names() {
    let b = Fake::start(Script::default());
    let a = Fake::start(Script {
        lan_url: Some(format!("{}/", b.origin())),
        ..Script::default()
    });
    let ran = run_within(
        args(&a.origin(), false, 1),
        PIN,
        Duration::from_secs(6),
        None,
    );
    assert!(ran.summary.complete, "{:?}", ran.summary);
    assert_eq!(a.seen(), ["/api/site"]);
    let mut on_b = b.seen();
    on_b.sort();
    assert_eq!(on_b, ["/api/auth", LISTEN, MIXER]);
    // It reads only: after its ListenStart, the one thing it sent on either
    // socket is the ListenStop at the end.
    assert_eq!(b.heard(), [format!("listen {STOP}")]);
}

#[test]
fn a_dropped_socket_ends_the_run_connection_lost_and_is_never_opened_again() {
    // The listen socket dropped, or closed by the server with a Close: the
    // mixer socket, still open, ends with a Close, and the client answers
    // the server's Close.
    let cases = [(false, &["mixer"][..]), (true, &["listen", "mixer"][..])];
    for (close_not_drop, closed) in cases {
        let fake = Fake::start(Script {
            drop_listen_after: Some(10),
            close_not_drop,
            ..Script::default()
        });
        let ran = run_within(
            args(&fake.origin(), true, 30),
            PIN,
            Duration::from_secs(5),
            None,
        );
        let s = &ran.summary;
        assert_eq!(s.error, Some(Reason::ConnectionLost), "{s:?}");
        assert!(!s.complete, "{s:?}");
        // Everything before the cut was counted, the refused frame included.
        assert_eq!((s.frames, s.decode_errors), (10, 1), "{s:?}");
        assert_eq!(s.reconnects, 0, "{s:?}");
        let seen = fake.seen();
        let counts = [times(&seen, MIXER), times(&seen, LISTEN)];
        assert_eq!(counts, [1, 1], "one connection per socket: {seen:?}");
        assert_eq!(fake.closed(closed.len()), closed, "{close_not_drop}");
    }
}

#[test]
fn a_socket_silent_past_the_idle_bound_ends_the_run_connection_lost() {
    let fake = Fake::start(Script {
        stall_mixer: true,
        ..Script::default()
    });
    let ran = run_within(
        args(&fake.origin(), true, 30),
        PIN,
        Duration::from_secs(6),
        None,
    );
    let s = &ran.summary;
    assert_eq!(s.error, Some(Reason::ConnectionLost), "{s:?}");
    assert!(!s.complete, "{s:?}");
    assert_eq!((s.meter_frames, s.reconnects), (0, 0), "{s:?}");
    // Not before the idle bound.
    assert!(ran.took >= limits().idle, "{:?}", ran.took);
    let seen = fake.seen();
    let counts = [times(&seen, MIXER), times(&seen, LISTEN)];
    assert_eq!(counts, [1, 1], "one connection per socket: {seen:?}");
    // Both sockets still end as at a whole run's end: the listen socket's
    // ListenStop reached the fake, and each ended with a Close.
    assert_eq!(fake.heard(), [format!("listen {STOP}")]);
    assert_eq!(fake.closed(2), ["listen", "mixer"]);
}

#[test]
fn a_peer_that_never_answers_the_close_is_left_after_the_close_wait() {
    let fake = Fake::start(Script {
        deaf_after_stop: true,
        ..Script::default()
    });
    let ran = run_within(
        args(&fake.origin(), true, 1),
        PIN,
        Duration::from_secs(6),
        None,
    );
    assert!(ran.summary.complete, "{:?}", ran.summary);
    // The listen socket waited its whole close wait after the run's second.
    let floor = Duration::from_secs(1) + limits().close_wait;
    assert!(ran.took >= floor, "{:?}", ran.took);
    assert_eq!(fake.heard(), [format!("listen {STOP}")]);
}

#[test]
fn a_refused_upgrade_ends_the_run_server_gone_without_a_second_try() {
    let fake = Fake::start(Script {
        refuse_upgrades: true,
        ..Script::default()
    });
    let ran = run_within(
        args(&fake.origin(), true, 30),
        PIN,
        Duration::from_secs(5),
        None,
    );
    assert_eq!(ran.summary.error, Some(Reason::ServerGone));
    assert!(!ran.summary.complete);
    assert_eq!(ran.summary.reconnects, 0);
    // Each socket was asked for once: nothing opens a socket again.
    let seen = fake.seen();
    let counts = [
        times(&seen, "/api/auth"),
        times(&seen, MIXER),
        times(&seen, LISTEN),
    ];
    assert_eq!(counts, [1, 1, 1], "{seen:?}");
}

#[test]
fn a_refused_login_ends_the_run_with_its_reason() {
    let not_engineer = Script {
        engineer: false,
        ..Script::default()
    };
    let cases = [
        (Script::default(), "9999", Reason::LoginRefused),
        (not_engineer, PIN, Reason::NotEngineer),
    ];
    for (script, pin, reason) in cases {
        let fake = Fake::start(script);
        // A 30 s run that ends at once: no socket, no wait.
        let ran = run_within(
            args(&fake.origin(), true, 30),
            pin,
            Duration::from_secs(5),
            None,
        );
        assert_eq!(ran.summary.error, Some(reason));
        assert!(!ran.summary.complete);
        let written: Vec<&Summary> = ran.written.iter().map(|(_, s)| s).collect();
        assert_eq!(written, [&ran.summary], "one summary, the end's");
        assert_eq!(fake.seen(), ["/api/auth"], "{reason:?}");
    }
}

#[test]
fn an_https_lan_url_is_refused() {
    // And a site that names no LAN URL is unreadable. Neither logs in.
    let cases = [
        (Some("https://mixer.example.org"), Reason::NotHttp),
        (None, Reason::SiteUnreadable),
    ];
    for (lan_url, reason) in cases {
        let fake = Fake::start(Script {
            lan_url: lan_url.map(str::to_owned),
            ..Script::default()
        });
        let ran = run_within(
            args(&fake.origin(), false, 30),
            PIN,
            Duration::from_secs(5),
            None,
        );
        assert_eq!(ran.summary.error, Some(reason));
        assert!(!ran.summary.complete);
        assert_eq!(fake.seen(), ["/api/site"], "{reason:?}");
    }
}

#[test]
fn the_summary_written_names_no_member_and_no_host() {
    let fake = Fake::start(Script::default());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("soakclient.json");
    let ran = run_within(
        args(&fake.origin(), true, 1),
        PIN,
        Duration::from_secs(6),
        Some(out.clone()),
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(serde_json::from_str::<Summary>(&text).unwrap(), ran.summary);
    for site in ["member9", "127.0.0.1", "member", "token"] {
        assert!(!text.contains(site), "{site}");
    }
}

#[test]
fn the_binary_exits_0_complete_1_with_a_reason_code_and_2_on_a_usage_error() {
    let fake = Fake::start(Script::default());
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("soakclient.json");
    let argv = |seconds: &str| -> Vec<String> {
        let origin = fake.origin();
        let out = out.to_str().unwrap();
        let base = origin.as_str();
        [
            "--base",
            base,
            "--direct",
            "--member",
            "member9",
            "--seconds",
            seconds,
            "--out",
            out,
        ]
        .map(str::to_owned)
        .to_vec()
    };
    // A usage error and a missing PIN: 2, before any request.
    assert_eq!(exe(Vec::new(), Some(PIN)).status.code(), Some(2));
    assert_eq!(exe(argv("30"), None).status.code(), Some(2));
    assert!(fake.seen().is_empty());
    // A refused login: 1, the reason code alone on stderr, the summary in
    // the file and on stdout.
    let refused = exe(argv("30"), Some("9999"));
    assert_eq!(refused.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(stderr.trim(), "iem-soakclient: login-refused");
    let file: Summary = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(file.error, Some(Reason::LoginRefused));
    assert_eq!(
        serde_json::from_slice::<Summary>(&refused.stdout).unwrap(),
        file
    );
    // A whole run: 0, nothing on stderr.
    let done = exe(argv("1"), Some(PIN));
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(
        serde_json::from_slice::<Summary>(&done.stdout)
            .unwrap()
            .complete
    );
    // A whole run whose final summary cannot be written: 1, never 0.
    let mut lost = argv("1");
    let missing = dir.path().join("missing").join("soakclient.json");
    *lost.last_mut().unwrap() = missing.to_str().unwrap().to_owned();
    let unwritten = exe(lost, Some(PIN));
    let stderr = String::from_utf8_lossy(&unwritten.stderr);
    assert_eq!(unwritten.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.trim(), "iem-soakclient: summary-unwritable");
    let summary: Summary = serde_json::from_slice(&unwritten.stdout).unwrap();
    assert!(summary.complete);
}
