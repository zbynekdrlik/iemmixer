//! The soak client against a fake server (S7 plan Task 6): the server's
//! login, `/api/site`, the mixer socket and the listen socket as the real
//! server serves them, on `127.0.0.1:0`, one thread per connection. Every
//! run is bounded: it runs on its own thread and the test waits for it with
//! `recv_timeout`, so a run that never ends fails here instead of hanging.
//! Timing is asserted only from below (a wait that must have happened) or
//! against the run's own measured length, never as a fixed sleep.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// The first listen socket sends this many frames and one frame Opus
    /// refuses, then it is dropped.
    drop_listen_after: Option<usize>,
    /// Each socket is dropped right after its first upgrade, and every later
    /// upgrade is refused.
    gone: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            lan_url: None,
            engineer: true,
            drop_listen_after: None,
            gone: false,
        }
    }
}

/// Upgrades the fake saw, per socket.
#[derive(Default)]
struct Upgrades {
    mixer: AtomicUsize,
    listen: AtomicUsize,
}

struct Fake {
    addr: SocketAddr,
    /// The target (path and query) of every request, HTTP and upgrade alike.
    seen: mpsc::Receiver<String>,
}

impl Fake {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, seen) = mpsc::channel();
        let shared = Arc::new((script, Upgrades::default()));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (shared, tx) = (Arc::clone(&shared), tx.clone());
                thread::spawn(move || connection(stream, &shared.0, &shared.1, &tx));
            }
        });
        Self { addr, seen }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The targets requested so far.
    fn seen(&self) -> Vec<String> {
        self.seen.try_iter().collect()
    }
}

fn connection(
    stream: TcpStream,
    script: &Script,
    upgrades: &Upgrades,
    seen: &mpsc::Sender<String>,
) {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let Some(head) = peek_head(&stream) else {
        return;
    };
    let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
    let _ = seen.send(target.clone());
    if head.to_ascii_lowercase().contains("upgrade: websocket") {
        upgrade(stream, &target, script, upgrades);
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

fn upgrade(stream: TcpStream, target: &str, script: &Script, upgrades: &Upgrades) {
    let (count, listen) = match target {
        MIXER => (&upgrades.mixer, false),
        LISTEN => (&upgrades.listen, true),
        // Another path or token: refused before the handshake.
        _ => return,
    };
    let n = count.fetch_add(1, Ordering::SeqCst);
    if script.gone && n > 0 {
        return;
    }
    let Ok(mut ws) = tungstenite::accept(stream) else {
        return;
    };
    if script.gone {
        return;
    }
    if listen {
        listen_stream(&mut ws, n == 0, script);
    } else {
        mixer_stream(&mut ws);
    }
}

/// Calls `send` every 20 ms on a fixed schedule until it returns false.
fn every_frame(mut send: impl FnMut() -> bool) {
    let mut next = Instant::now();
    while send() {
        next += FRAME;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

/// `Hello`, `State`, a silence, then `Meters` every 20 ms until the client
/// is gone. The fake reads nothing: the client sends nothing here.
fn mixer_stream(ws: &mut WebSocket<TcpStream>) {
    let hello = r#"{"event":"Hello","data":{"proto":2,"build":"local","min_client_proto":2}}"#;
    let state = r#"{"event":"State","data":{"channels":[],"connected":true}}"#;
    for text in [hello, state] {
        if ws.send(Message::text(text)).is_err() {
            return;
        }
    }
    thread::sleep(SILENCE);
    let meters = r#"{"event":"Meters","data":{"meters":{"mic1":[0.1,0.1]}}}"#;
    every_frame(|| ws.send(Message::text(meters)).is_ok());
}

/// Waits for `ListenStart` on member9, answers `listening`, then sends one
/// Opus packet of silence every 20 ms until the client is gone (the first
/// socket of a `drop_listen_after` script is cut short).
fn listen_stream(ws: &mut WebSocket<TcpStream>, first: bool, script: &Script) {
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
    let cut = script.drop_listen_after.filter(|_| first);
    let mut sent = 0;
    every_frame(|| {
        if cut == Some(sent) {
            // A frame Opus refuses (63 frames of 20 ms in one packet), then
            // the socket is dropped.
            let _ = ws.send(Message::binary(vec![0xff; 3]));
            return false;
        }
        sent += 1;
        let packet = encoder.encode_vec_float(&[0.0; 1920], 4000).unwrap();
        ws.send(Message::binary(packet)).is_ok()
    });
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

/// The tests' bounds: a socket down for 1 s ends the run, a summary every
/// 500 ms, reads wait 100 ms.
fn limits() -> Limits {
    Limits {
        give_up: Duration::from_secs(1),
        write_every: Duration::from_millis(500),
        read_timeout: Duration::from_millis(100),
    }
}

struct Ran {
    summary: Summary,
    /// Every summary `run` handed to its writer, in order.
    written: Vec<Summary>,
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
            written.push(s.clone());
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

/// The binary with `argv` and the PIN in its environment when given, waited
/// for at most 8 s.
fn exe(argv: Vec<String>, pin: Option<&'static str>) -> Output {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iem-soakclient"));
        cmd.args(&argv).env_remove(PIN_ENV);
        if let Some(pin) = pin {
            cmd.env(PIN_ENV, pin);
        }
        let _ = tx.send(cmd.output());
    });
    let out = rx.recv_timeout(Duration::from_secs(8));
    out.expect("the binary ends within 8 s").unwrap()
}

#[test]
fn a_run_counts_frames_meters_one_reconnect_and_one_bad_frame() {
    let fake = Fake::start(Script {
        drop_listen_after: Some(10),
        ..Script::default()
    });
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
    // The listen socket's drop is the one reconnect: the mixer socket's
    // silence (longer than a read's wait) did not close it.
    assert_eq!(s.reconnects, 1, "{s:?}");
    assert_eq!(s.decode_errors, 1, "{s:?}");
    assert!(s.gaps >= 1, "{s:?}");
    // The reopen waited its backoff (1 s) first.
    assert!(s.max_gap_ms >= 1_000, "{s:?}");
    assert!(s.frames >= 60, "{s:?}");
    assert!(s.expected_frames > s.frames, "{s:?}");
    assert!(s.meter_frames >= 60, "{s:?}");
    assert!(s.first_frame_ms.is_some(), "{s:?}");
    assert_eq!(s.no_source, 0);
    // A summary every 500 ms, the last one the end's.
    assert_eq!(ran.written.last(), Some(s));
    let most = ran.took.as_secs_f64() / 0.5 + 2.0;
    let writes = ran.written.len();
    assert!(writes >= 2 && writes as f64 <= most, "{writes} writes");
    assert!(ran.written.iter().rev().skip(1).all(|w| !w.complete));
    // One login, the mixer socket once, the listen socket twice.
    let seen = fake.seen();
    let times = |target: &str| seen.iter().filter(|t| *t == target).count();
    assert_eq!((times("/api/auth"), times(MIXER), times(LISTEN)), (1, 1, 2));
    assert_eq!(times("/api/site"), 0, "--direct reads no /api/site");
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
        assert_eq!(ran.written, [ran.summary.clone()], "one summary, the end's");
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
fn a_server_that_stays_gone_ends_the_run_after_the_give_up_bound() {
    let fake = Fake::start(Script {
        gone: true,
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
    assert_eq!(ran.summary.reconnects, 0, "no reopen succeeded");
    // Not before the bound: the reopen waited 1 s and failed.
    assert!(ran.took >= Duration::from_secs(1), "{:?}", ran.took);
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
}
