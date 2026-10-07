//! The soak client's wire (S7 design note §4): the login and `/api/site`
//! over plain HTTP, the mixer and listen sockets each on its own thread, the
//! run's clock on the calling thread. A socket read waits at most
//! `read_timeout`, so every thread sees the end of the run within it; the
//! clock and the reopen waits sleep on a condition variable. No thread
//! polls. Nothing here ends a process: the sockets close by being dropped,
//! the listen socket after its `ListenStop`.

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
    Args, Reason, Summary, backoff, classify, listen_path, listen_start, mixer_path, origin, ws_url,
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
    /// A socket that cannot be opened again for this long ends the run
    /// (`server-gone`).
    pub give_up: Duration,
    /// The summary is handed to the writer this often, and at the end.
    pub write_every: Duration,
    /// Socket reads wait this long, so the threads see the end within it.
    pub read_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            give_up: Duration::from_secs(120),
            write_every: Duration::from_secs(60),
            read_timeout: Duration::from_millis(500),
        }
    }
}

/// One run: the login (once), then the mixer and listen sockets on their own
/// threads until `args.seconds` have passed since the first open (complete)
/// or a socket stays gone past `give_up`. `write` gets the summary every
/// `write_every` and the final one; a run that fails before its sockets
/// writes that one summary only.
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
        s.spawn(|| keep_open(&shared, &mixer, limits, &mut Role::Mixer));
        s.spawn(|| keep_open(&shared, &listen, limits, &mut Role::listen(&args.member)));
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

    /// Counts what the clock waits for (an open, a give-up) and wakes it.
    fn signal(&self, f: impl FnOnce(&mut Tally)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Waits `wait`, or less when the run ends; true when it has ended.
    fn pause(&self, wait: Duration) -> bool {
        let tally = self.lock();
        let waited = self
            .changed
            .wait_timeout_while(tally, wait, |_| !self.stopped());
        drop(waited.unwrap_or_else(PoisonError::into_inner));
        self.stopped()
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
/// or a socket gave up.
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

/// One socket's reopen clock: failed opens in a row, and since when the
/// socket has been down.
struct Reopen {
    down_since: Instant,
    failures: u32,
}

impl Reopen {
    /// Not open yet: down since `now`.
    fn new(now: Instant) -> Self {
        Self {
            down_since: now,
            failures: 0,
        }
    }

    fn opened(&mut self) {
        self.failures = 0;
    }

    fn closed(&mut self, now: Instant) {
        self.down_since = now;
    }

    /// An open failed at `now`: false once the socket has been down for
    /// `give_up` (the run ends), true to try again after [`Reopen::wait`].
    fn failed(&mut self, now: Instant, give_up: Duration) -> bool {
        self.failures = self.failures.saturating_add(1);
        now.saturating_duration_since(self.down_since) < give_up
    }

    /// The wait before the next open.
    fn wait(&self) -> Duration {
        backoff(self.failures)
    }
}

/// One socket for the whole run: opened, read until it closes, opened again
/// after its backoff; down for `give_up`, it ends the run (`server-gone`).
fn keep_open(shared: &Shared, url: &str, limits: &Limits, role: &mut Role) {
    let mut reopen = Reopen::new(Instant::now());
    let mut again = false;
    while !shared.stopped() {
        if let Some(mut socket) = open(url, limits.read_timeout) {
            reopen.opened();
            shared.signal(|t| t.opened(Instant::now(), again));
            again = true;
            role.serve(&mut socket, shared);
            reopen.closed(Instant::now());
        } else if !reopen.failed(Instant::now(), limits.give_up) {
            shared.signal(|t| t.fail(Reason::ServerGone));
            return;
        }
        if shared.pause(reopen.wait()) {
            return;
        }
    }
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
    /// The member's mixer page: its `Meters` are counted. It sends nothing.
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

    /// Reads `socket` into the tally until it closes or the run ends (the
    /// listen socket then sends `ListenStop`).
    fn serve(&mut self, socket: &mut Socket, shared: &Shared) {
        if let Role::Listen { start, .. } = self {
            if socket.send(Message::text(start.clone())).is_err() {
                return;
            }
            shared.count(|t| t.listen_started(Instant::now()));
        }
        let mut pcm = [0f32; 2 * SAMPLES];
        while !shared.stopped() {
            match socket.read() {
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
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) => {
                    if !waited(&e) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if matches!(self, Role::Listen { .. }) {
            // The socket is dropped next whether this reaches the server or not.
            let _ = socket.send(Message::text(LISTEN_STOP));
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
        assert_eq!((l.give_up, l.write_every), (secs(120), secs(60)));
        assert_eq!(l.read_timeout, Duration::from_millis(500));
        // Far above the server's meter period and its 5 s `no_source`
        // repeat on an open listen socket.
        assert_eq!(l.idle, secs(10));
    }

    #[test]
    fn a_socket_reopens_after_its_backoff_and_gives_up_once_down_for_the_bound() {
        let t0 = Instant::now();
        let bound = Duration::from_secs(1);
        let at = |ms| t0 + Duration::from_millis(ms);
        let secs = Duration::from_secs;
        // Never opened: down since the start.
        assert!(!Reopen::new(t0).failed(at(1_000), bound));
        let mut r = Reopen::new(t0);
        assert_eq!(r.wait(), secs(1));
        assert!(r.failed(at(999), bound), "down 999 ms: open again");
        assert_eq!(r.wait(), secs(2));
        assert!(r.failed(at(999), bound));
        assert_eq!(r.wait(), secs(4));
        // An open resets the backoff; the down clock starts at the close.
        r.opened();
        assert_eq!(r.wait(), secs(1));
        r.closed(at(5_000));
        assert!(r.failed(at(5_999), bound));
        assert!(
            !r.failed(at(6_000), bound),
            "down exactly the bound: give up"
        );
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
