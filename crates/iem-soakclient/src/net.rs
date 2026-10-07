//! The soak client's wire (S7 design note §4): the login and `/api/site`
//! over plain HTTP, the mixer and listen sockets each on its own thread, the
//! run's clock on the calling thread. A socket read waits at most
//! `read_timeout`, so a reading thread sees the end of the run within it (a
//! thread inside an open, within the open's own bounds); the clock sleeps on
//! a condition variable. No thread polls.
//!
//! Each socket is opened once and never again (#10): after "ide event" the
//! predecessor app answers at the same address, takes the client's token
//! and would take a new `ListenStart`. A socket that cannot be opened ends
//! the run (`server-gone`); its first close, an error on it, or a silence of
//! `idle` ends the run (`connection-lost`). The server sends meters and
//! repeats a listen's `no_source`, so silence means it stopped serving that
//! socket. Nothing here ends a process: each socket ends with the listen
//! socket's `ListenStop`, a Close and a bounded wait for the peer's Close,
//! and is then dropped.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

use crate::tally::{SAMPLES, Tally};
use crate::{
    Args, Reason, Summary, classify, listen_path, listen_start, mixer_path, origin, ws_url,
};

/// One HTTP request (`/api/site`, the login) at most.
const HTTP_WAIT: Duration = Duration::from_secs(10);
/// A socket's connect, its handshake and each of its writes at most.
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// The login's member: the listen socket is the engineer's only.
const ENGINEER: &str = "engineer";
/// `ClientMsg::ListenStop`.
const LISTEN_STOP: &str = r#"{"cmd":"ListenStop"}"#;

/// The run's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The summary is handed to the writer this often, and at the end.
    pub write_every: Duration,
    /// Socket reads wait this long, so the threads see the end within it.
    pub read_timeout: Duration,
    /// A socket that has sent nothing for this long ends the run
    /// (`connection-lost`).
    pub idle: Duration,
    /// At its end a socket waits at most this long for the peer's Close.
    pub close_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            write_every: Duration::from_secs(60),
            read_timeout: Duration::from_millis(500),
            idle: Duration::from_secs(10),
            close_wait: Duration::from_secs(2),
        }
    }
}

/// One run: the login (once), then the mixer and listen sockets on their own
/// threads until `args.seconds` have passed since the first open (complete)
/// or a socket could not be opened or was lost. `write` gets the summary
/// every `write_every` and the final one, once both sockets have ended; a
/// run that fails before its sockets writes that one summary only.
pub fn run(args: &Args, pin: &str, limits: &Limits, write: &mut dyn FnMut(&Summary)) -> Summary {
    let (origin, token) = match login(args, pin) {
        Ok(found) => found,
        Err(reason) => {
            let mut tally = Tally::default();
            tally.fail(reason);
            let summary = tally.summary(Instant::now(), false);
            write(&summary);
            return summary;
        }
    };
    let mixer = ws_url(&origin, &mixer_path(&args.member, &token));
    let listen = ws_url(&origin, &listen_path(&token));
    let shared = Shared::default();
    let summary = thread::scope(|s| {
        s.spawn(|| hold(&shared, &mixer, limits, &mut Role::Mixer));
        s.spawn(|| hold(&shared, &listen, limits, &mut Role::listen(&args.member)));
        let summary = watch(&shared, args.seconds, limits.write_every, write);
        shared.end();
        summary
    });
    write(&summary);
    summary
}

/// The origin the sockets go to and the engineer's token: `/api/site`'s
/// LAN URL at `--base` (or `--base` itself with `--direct`), then the login
/// there. A server that does not answer the login is `server-gone`.
fn login(args: &Args, pin: &str) -> Result<(String, String), Reason> {
    let http = agent();
    let lan_url = if args.direct {
        None
    } else {
        site_lan_url(&http, &args.base)
    };
    let origin = origin(args, lan_url.as_deref())?;
    let body = serde_json::json!({"member": ENGINEER, "pin": pin}).to_string();
    let mut reply = http
        .post(format!("{origin}/api/auth"))
        .content_type("application/json")
        .send(&body)
        .map_err(|_| Reason::ServerGone)?;
    if reply.status() != 200 {
        return Err(Reason::LoginRefused);
    }
    let text = reply
        .body_mut()
        .read_to_string()
        .map_err(|_| Reason::LoginRefused)?;
    let login: Login = serde_json::from_str(&text).map_err(|_| Reason::LoginRefused)?;
    if !login.engineer {
        return Err(Reason::NotEngineer);
    }
    Ok((origin, login.token))
}

/// The login's answer (`LoginResponse`; the other fields are not read).
#[derive(Deserialize)]
struct Login {
    token: String,
    engineer: bool,
}

/// `/api/site`'s answer (`SiteLinks`).
#[derive(Deserialize)]
struct Site {
    lan_url: Option<String>,
}

/// `/api/site`'s `lan_url` at `base`; `None` when it cannot be read.
fn site_lan_url(http: &ureq::Agent, base: &str) -> Option<String> {
    let mut reply = http.get(format!("{base}/api/site")).call().ok()?;
    if reply.status() != 200 {
        return None;
    }
    let text = reply.body_mut().read_to_string().ok()?;
    serde_json::from_str::<Site>(&text).ok()?.lan_url
}

/// Plain HTTP on the LAN, as the guard's: one bound per request, any status
/// read as an answer, no redirect, no proxy (the sockets use none either).
fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_WAIT))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    ureq::Agent::new_with_config(config)
}

/// What the threads share: the counts, their change signal and the end.
#[derive(Default)]
struct Shared {
    tally: Mutex<Tally>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Tally> {
        self.tally.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts into the tally (a frame, a text frame).
    fn count(&self, f: impl FnOnce(&mut Tally)) {
        f(&mut self.lock());
    }

    /// Counts what the clock waits for (an open, a failure) and wakes it.
    fn signal(&self, f: impl FnOnce(&mut Tally)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Ends the run: the threads see it within a read's wait.
    fn end(&self) {
        let _tally = self.lock();
        self.stop.store(true, Ordering::Release);
        self.changed.notify_all();
    }
}

/// The run's clock: hands `write` a summary every `write_every`, and returns
/// the final one once `seconds` have passed since the first open (complete)
/// or the run failed.
fn watch(
    shared: &Shared,
    seconds: u64,
    write_every: Duration,
    write: &mut dyn FnMut(&Summary),
) -> Summary {
    let mut next_write = Instant::now() + write_every;
    let mut tally = shared.lock();
    loop {
        let now = Instant::now();
        if tally.error().is_some() {
            return tally.summary(now, false);
        }
        let end = tally.ends_at(seconds);
        if end.is_some_and(|end| now >= end) {
            return tally.summary(now, true);
        }
        if now >= next_write {
            let summary = tally.summary(now, false);
            drop(tally);
            write(&summary);
            next_write = Instant::now() + write_every;
            tally = shared.lock();
            continue;
        }
        let wake = end.map_or(next_write, |end| end.min(next_write));
        let wait = wake.saturating_duration_since(now);
        let woken = shared.changed.wait_timeout(tally, wait);
        tally = woken.unwrap_or_else(PoisonError::into_inner).0;
    }
}

/// One socket for the whole run, opened once. Not opened, it ends the run
/// at once (`server-gone`): a second try could reach whatever answers at
/// that address by then (the predecessor app after "ide event"). Lost
/// before the end, it ends the run (`connection-lost`). Either way it then
/// ends as [`Role::finish`] says, as far as it still can.
fn hold(shared: &Shared, url: &str, limits: &Limits, role: &mut Role) {
    let Some(mut socket) = open(url, limits.read_timeout) else {
        shared.signal(|t| t.fail(Reason::ServerGone));
        return;
    };
    shared.signal(|t| t.opened(Instant::now()));
    if !role.serve(&mut socket, shared, limits.idle) {
        shared.signal(|t| t.fail(Reason::ConnectionLost));
    }
    role.finish(&mut socket, limits.close_wait);
}

type Socket = WebSocket<TcpStream>;

/// Opens the plain `ws://` socket at `url`: its connect, handshake and
/// writes bounded by [`OPEN_WAIT`], its reads then by `read_timeout`.
/// `None`: not open.
fn open(url: &str, read_timeout: Duration) -> Option<Socket> {
    let request = url.into_client_request().ok()?;
    let host = request
        .uri()
        .host()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let addrs = (host, request.uri().port_u16().unwrap_or(80)).to_socket_addrs();
    let stream = addrs
        .ok()?
        .find_map(|addr| TcpStream::connect_timeout(&addr, OPEN_WAIT).ok())?;
    stream.set_read_timeout(Some(OPEN_WAIT)).ok()?;
    stream.set_write_timeout(Some(OPEN_WAIT)).ok()?;
    stream.set_nodelay(true).ok()?;
    let (mut socket, _) = tungstenite::client(request, stream).ok()?;
    socket.get_mut().set_read_timeout(Some(read_timeout)).ok()?;
    Some(socket)
}

/// A read that only waited out its timeout (`WouldBlock` on Unix,
/// `TimedOut` on Windows); the socket is still open.
fn waited(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// What a socket is for.
enum Role {
    /// The member's mixer page: its `Meters` are counted. It sends nothing
    /// but its Close.
    Mixer,
    /// The engineer's listen socket on the member's mix: `start` is its
    /// `ListenStart`; a decoder that could not be made leaves every frame
    /// a decode error.
    Listen {
        start: String,
        decoder: Option<opus::Decoder>,
    },
}

impl Role {
    fn listen(member: &str) -> Self {
        Role::Listen {
            start: listen_start(member),
            decoder: opus::Decoder::new(48_000, opus::Channels::Stereo).ok(),
        }
    }

    /// Reads `socket` into the tally until the run ends (true), or until it
    /// closes, fails or stays silent for `idle` before that (false: lost).
    fn serve(&mut self, socket: &mut Socket, shared: &Shared, idle: Duration) -> bool {
        if let Role::Listen { start, .. } = self {
            if socket.send(Message::text(start.clone())).is_err() {
                return false;
            }
            shared.count(|t| t.listen_started(Instant::now()));
        }
        let mut pcm = [0f32; 2 * SAMPLES];
        let mut heard = Instant::now();
        while !shared.stopped() {
            let read = socket.read();
            if read.is_ok() {
                heard = Instant::now();
            }
            match read {
                Ok(Message::Binary(data)) => {
                    if let Role::Listen { decoder, .. } = self {
                        let samples = decoder
                            .as_mut()
                            .and_then(|d| d.decode_float(&data, &mut pcm, false).ok());
                        shared.count(|t| t.frame(Instant::now(), samples));
                    }
                }
                Ok(Message::Text(text)) => {
                    let event = classify(&text);
                    shared.count(|t| t.text(&event));
                }
                // The peer's Close: [`Role::finish`] sends the answer.
                Ok(Message::Close(_)) => return false,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) => {
                    // A read's wait ran out: the socket stays open unless it
                    // has been silent for `idle`.
                    if !waited(&e) || heard.elapsed() >= idle {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// Ends `socket` so that what it sent reaches the peer: the listen
    /// socket's `ListenStop`, a Close (or the answer to the peer's), then
    /// reads, not counted, until the peer's Close or the socket's end, for
    /// `wait` and at most one read's wait more. Once the peer's Close has
    /// come nothing is left unread, so the drop that follows ends the
    /// connection with a FIN on Windows, not with a reset, which can lose
    /// the `ListenStop` at the peer. Best effort: a socket that already
    /// failed ends at its first error.
    fn finish(&self, socket: &mut Socket, wait: Duration) {
        if matches!(self, Role::Listen { .. }) {
            let _ = socket.send(Message::text(LISTEN_STOP));
        }
        let _ = socket.close(None);
        let began = Instant::now();
        while began.elapsed() <= wait {
            match socket.read() {
                Ok(Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) => {
                    if !waited(&e) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_limits() {
        let l = Limits::default();
        let secs = Duration::from_secs;
        assert_eq!(l.write_every, secs(60));
        assert_eq!(l.read_timeout, Duration::from_millis(500));
        // Far above the server's meter period and its 5 s `no_source`
        // repeat on an open listen socket.
        assert_eq!(l.idle, secs(10));
        // The peer's Close answers at once; the wait only bounds a peer
        // that never sends it.
        assert_eq!(l.close_wait, secs(2));
    }

    #[test]
    fn only_a_read_timeout_is_a_wait() {
        // WouldBlock on Unix, TimedOut on Windows.
        assert!(waited(&io::ErrorKind::WouldBlock.into()));
        assert!(waited(&io::ErrorKind::TimedOut.into()));
        for closed in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::BrokenPipe,
        ] {
            assert!(!waited(&closed.into()), "{closed:?}");
        }
    }

    #[test]
    fn the_listen_stop_is_the_servers_own() {
        let stop = serde_json::to_string(&iem_core::ClientMsg::ListenStop).unwrap();
        assert_eq!(LISTEN_STOP, stop);
    }
}
