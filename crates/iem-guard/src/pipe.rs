//! The guard pipe's transport (S6 design note §5.1): the listener, one
//! thread per connection, and the client `iemmode` uses.
//!
//! On Windows the pipe has the engine's hardening (plan Task 6): remote
//! clients refused and the first instance only (the local-socket defaults),
//! and a protected DACL for the logged-on user and SYSTEM. Pipes are global,
//! so `iemmode` in an ssh session (session 0) reaches the guard in session
//! 1. On Linux (the tests) it is a Unix socket file.
//!
//! A connection sends requests and reads one reply each; the routing is
//! [`Shared::route`]: while a switch runs, "ide event" pre-empts or waits
//! for it and everything else but `Status` is refused; otherwise the
//! request goes to the daemon thread. `Subscribe` turns the connection into
//! a stream of [`Update`]s (the tray): the state, and the tray's quit.
//!
//! The guard never waits long for a client: every reply and update goes out
//! through `bounded`, so a client that takes nothing for [`SEND_TIMEOUT`]
//! fails the write and its connection ends, and on Windows the guard's end
//! closes as it is dropped, whether or not the client read what came last
//! (`iem_win::pipe::write_within`, the engine's writer too).

use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, Name, Stream,
};
use tracing::{info, warn};

use crate::daemon::{Job, Route, Shared};
use crate::proto::{self, FrameError, Reply, Request, Update};

#[cfg(windows)]
mod win;
// The bounded, clean writer on Windows: the supervisor connection's sends
// to the engine use it too (`win::engine`).
#[cfg(windows)]
pub(crate) use win::Bounded;

/// An idle listener looks for a new connection this often.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// "ide event" waits at most this long for the switch in progress.
pub const AWAIT_END: Duration = Duration::from_secs(600);
/// A subscriber looks at the view at least this often.
pub const SUBSCRIBER_POLL: Duration = Duration::from_secs(5);
/// A new guard tries to create the pipe's first instance this long (the
/// old guard's instances end with its process, after a hand-over)…
pub const LISTEN_WAIT: Duration = Duration::from_secs(10);
/// …this often.
pub const LISTEN_EVERY: Duration = Duration::from_millis(250);
/// A stopping guard waits this long for its last reply to be written.
pub const LAST_REPLY: Duration = Duration::from_secs(5);
/// A client that takes nothing for this long fails the write and its
/// connection ends (`bounded`): well inside [`LAST_REPLY`], so a stopping
/// guard's last reply is written or given up before that wait ends.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// A socket path on Unix, a pipe name on Windows.
pub fn pipe_name(name: &str) -> io::Result<Name<'static>> {
    let owned = name.to_owned();
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        owned.to_fs_name::<GenericFilePath>()
    }
    #[cfg(not(unix))]
    {
        use interprocess::local_socket::GenericNamespaced;
        owned.to_ns_name::<GenericNamespaced>()
    }
}

/// The pipe's protected DACL: the logged-on user and SYSTEM, nobody else
/// (S3 hand-off, spec §2.3).
pub fn sddl_for(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
}

/// The guard's listener; `accept` does not block (the acceptor polls its
/// stop flag).
#[cfg(windows)]
pub fn listen(name: &str) -> io::Result<Listener> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    let sid = iem_win::token::current_user_sid()?;
    let sddl = U16CString::from_str(sddl_for(&sid))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let sd = SecurityDescriptor::deserialize(&sddl)?;
    ListenerOptions::new()
        .name(pipe_name(name)?)
        .nonblocking(ListenerNonblockingMode::Accept)
        .security_descriptor(sd)
        .create_sync()
}

/// The guard's listener; a leftover socket file is replaced (the guard's
/// mutex already made this the only guard).
#[cfg(not(windows))]
pub fn listen(name: &str) -> io::Result<Listener> {
    ListenerOptions::new()
        .name(pipe_name(name)?)
        .nonblocking(ListenerNonblockingMode::Accept)
        .try_overwrite(true)
        .create_sync()
}

/// `f` until it succeeds, every `every`, for up to `limit`; then its last
/// error. After a hand-over the old guard's pipe instances live on until
/// its process has ended, so the new guard's first instance waits for them.
pub fn retry<T>(
    what: &str,
    limit: Duration,
    every: Duration,
    mut f: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let start = Instant::now();
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if start.elapsed() < limit => {
                warn!("{what}: {e}; trying again");
                thread::sleep(every);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Accepts connections until `stop`, each in a thread of its own.
pub fn serve(
    listener: Listener,
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    stop: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("iemmixer-guard-pipe".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok(stream) => spawn_connection(stream, &shared, &jobs),
                    Err(e) => {
                        if e.kind() != io::ErrorKind::WouldBlock {
                            warn!("the guard pipe: {e}");
                        }
                        thread::sleep(ACCEPT_POLL);
                    }
                }
            }
            info!("the guard pipe stops");
        })
}

/// A connection's writing side, which never waits long for the client: a
/// Unix stream waits at most its send timeout ([`SEND_TIMEOUT`], set at
/// accept); a Windows pipe has no timeouts, so each write is issued
/// overlapped and cancelled when the client has not taken it within
/// [`SEND_TIMEOUT`] (`iem_win::pipe::write_within`). Either way the write
/// then fails and the connection ends. A Windows write through it also
/// leaves the stream out of interprocess's flush on drop, whose one thread
/// per process waits for each client to read everything before it closes
/// the next stream: the guard's end closes as it is dropped. Always write
/// through it, never through `&Stream`.
fn bounded(stream: &Stream) -> impl Write + '_ {
    #[cfg(windows)]
    {
        Bounded::new(stream, SEND_TIMEOUT)
    }
    #[cfg(not(windows))]
    {
        stream
    }
}

fn spawn_connection(stream: Stream, shared: &Arc<Shared>, jobs: &Sender<Job>) {
    if let Err(e) = stream.set_nonblocking(false) {
        warn!("a guard pipe connection: {e}");
        return;
    }
    // A Windows pipe refuses timeouts: its writes are bounded by `bounded`.
    #[cfg(unix)]
    if let Err(e) = stream.set_send_timeout(Some(SEND_TIMEOUT)) {
        warn!("a guard pipe connection: {e}");
        return;
    }
    let (shared, jobs) = (Arc::clone(shared), jobs.clone());
    let spawned = thread::Builder::new()
        .name("iemmixer-guard-client".to_owned())
        .spawn(move || connection(&stream, &shared, &jobs));
    if let Err(e) = spawned {
        warn!("a guard pipe connection's thread: {e}");
    }
}

/// Hands `req` to the daemon thread and waits for its reply.
fn ask(jobs: &Sender<Job>, req: Request, epoch: u64) -> Result<Reply, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    jobs.send(Job {
        req,
        epoch,
        reply: tx,
    })
    .map_err(|_| "the guard is stopping".to_owned())?;
    rx.recv()
        .map_err(|_| "the guard dropped the request".to_owned())
}

fn connection(stream: &Stream, shared: &Shared, jobs: &Sender<Job>) {
    let mut wire = stream;
    let mut out = bounded(stream);
    loop {
        let req: Request = match proto::read_msg(&mut wire) {
            Ok(req) => req,
            Err(FrameError::Closed) => return,
            Err(FrameError::Bad(why)) => {
                let reply = shared.view().reply(false, &format!("bad request: {why}"));
                if let Err(e) = proto::write_frame(&mut out, &reply) {
                    info!("a guard pipe client: {e}");
                }
                return;
            }
            Err(e) => {
                info!("a guard pipe client: {e}");
                return;
            }
        };
        // A reply the daemon thread handed over is counted once written, so
        // a stopping guard's last reply reaches its client first.
        let (reply, handed) = match shared.route(&req) {
            Route::Now(reply) => (reply, false),
            Route::AwaitEnd(note) => (shared.await_end(note, AWAIT_END), false),
            Route::Queue(epoch) => match ask(jobs, req, epoch) {
                Ok(reply) => (reply, true),
                Err(why) => (shared.view().reply(false, &why), false),
            },
            Route::Subscribe => {
                subscribe(stream, shared);
                return;
            }
        };
        let written = proto::write_frame(&mut out, &reply);
        if handed {
            shared.reply_done();
        }
        if let Err(e) = written {
            info!("a guard pipe client left: {e}");
            return;
        }
    }
}

/// A subscriber (the tray): the state now, then every change, and
/// `Update::Quit` when the guard stops the tray: at once, or right after
/// the first state when the quit came while no tray was subscribed.
fn subscribe(stream: &Stream, shared: &Shared) {
    shared.add_subscriber();
    let mut wire = bounded(stream);
    let first = shared.view();
    let mut seen = (first.version, first.tray_quits);
    let state = |v: &crate::daemon::View| Update::State(v.reply(true, &v.status));
    let mut open = proto::write_update(&mut wire, &state(&first)).is_ok();
    while open {
        if shared.take_tray_quit() && proto::write_update(&mut wire, &Update::Quit).is_err() {
            break;
        }
        let v = shared.wait_change(seen, SUBSCRIBER_POLL);
        let changed = v.version != seen.0;
        seen = (v.version, v.tray_quits);
        open = !changed || proto::write_update(&mut wire, &state(&v)).is_ok();
    }
    shared.drop_subscriber();
    info!("a subscriber left");
}

/// Why a call to the guard failed.
#[derive(Debug)]
pub enum CallError {
    /// Nothing answers on the pipe (the request was not sent).
    Connect(io::Error),
    /// The request was sent, the reply did not come.
    Pipe(FrameError),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "no guard on the pipe: {e}"),
            Self::Pipe(e) => write!(f, "the guard pipe broke: {e}"),
        }
    }
}

impl std::error::Error for CallError {}

/// One request and its reply (`iemmode`).
pub fn call(name: &str, req: &Request) -> Result<Reply, CallError> {
    let stream = pipe_name(name)
        .and_then(Stream::connect)
        .map_err(CallError::Connect)?;
    let mut wire = &stream;
    proto::write_frame(&mut wire, req).map_err(CallError::Pipe)?;
    proto::read_msg(&mut wire).map_err(CallError::Pipe)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    use super::*;
    use crate::cancel::Cancel;
    use crate::daemon::{Guard, Outcome, handle};
    use crate::pc::fake::FakePc;
    use crate::plan::{Facts, Mode};
    use crate::proto::Update;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    /// A socket in `dir` on Unix, a unique pipe name on Windows.
    fn test_name(dir: &std::path::Path) -> String {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        if cfg!(unix) {
            dir.join(format!("guard-{n}.sock"))
                .to_string_lossy()
                .into_owned()
        } else {
            format!("iemmixer-guard-test-{}-{n}", std::process::id())
        }
    }

    /// A served pipe whose requests a test thread answers with `handle`.
    struct Served {
        name: String,
        shared: Arc<Shared>,
        cancel: Cancel,
        stop: Arc<AtomicBool>,
        _dir: tempfile::TempDir,
    }

    fn served() -> (Served, Receiver<Job>) {
        let dir = tempfile::tempdir().unwrap();
        let name = test_name(dir.path());
        let listener = listen(&name).unwrap();
        let cancel = Cancel::default();
        let shared = Arc::new(Shared::new(cancel.clone()));
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        serve(listener, Arc::clone(&shared), tx, Arc::clone(&stop)).unwrap();
        (
            Served {
                name,
                shared,
                cancel,
                stop,
                _dir: dir,
            },
            rx,
        )
    }

    impl Drop for Served {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn the_dacl_is_the_user_and_system_only() {
        assert_eq!(
            sddl_for("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)"
        );
    }

    #[test]
    fn status_comes_from_the_view_and_the_rest_from_the_daemon() {
        let (s, rx) = served();
        s.shared.update(|v| {
            v.mode = Mode::Dev;
            v.status = "mode dev; no bundle".into();
        });
        let status = call(&s.name, &Request::Status).unwrap();
        assert!(status.ok);
        assert_eq!(status.mode, Mode::Dev);
        assert_eq!(status.detail, "mode dev; no bundle");
        // The daemon side: a guard answers what reaches it.
        let answer = thread::spawn(move || {
            let job = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut pc = FakePc::new(Facts::default());
            let mut g = Guard::for_test(Mode::Event);
            let epoch = job.epoch;
            job.reply
                .send(handle(&mut pc, &mut g, job.req, epoch))
                .unwrap();
            epoch
        });
        let reply = call(&s.name, &Request::AlarmAck { id: 99 }).unwrap();
        assert!(!reply.ok);
        assert_eq!(reply.detail, "no alarm 99");
        assert_eq!(answer.join().unwrap(), 0);
    }

    #[test]
    fn a_queued_reply_is_counted_once_written() {
        let (s, rx) = served();
        let answer = thread::spawn(move || {
            let job = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut pc = FakePc::new(Facts::default());
            let mut g = Guard::for_test(Mode::Event);
            let epoch = job.epoch;
            job.reply
                .send(handle(&mut pc, &mut g, job.req, epoch))
                .unwrap();
        });
        // Answered from the view: nothing to count.
        assert!(call(&s.name, &Request::Status).unwrap().ok);
        let reply = call(&s.name, &Request::AlarmAck { id: 99 }).unwrap();
        answer.join().unwrap();
        assert_eq!(reply.detail, "no alarm 99");
        let t = Instant::now();
        while s.shared.view().replies_done == 0 {
            assert!(t.elapsed() < Duration::from_secs(5), "never counted");
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(s.shared.view().replies_done, 1);
    }

    #[test]
    fn a_busy_pipe_name_is_tried_again_until_the_limit() {
        let mut tries = 0;
        let t = Instant::now();
        let got = retry(
            "the guard pipe",
            Duration::from_secs(5),
            Duration::from_millis(20),
            || {
                tries += 1;
                if tries < 3 {
                    Err(io::Error::other("access denied"))
                } else {
                    Ok(tries)
                }
            },
        );
        assert_eq!(got.unwrap(), 3);
        assert!(
            t.elapsed() >= Duration::from_millis(40),
            "{:?}",
            t.elapsed()
        );
        // The limit ends it with the last error.
        let mut tries = 0;
        let t = Instant::now();
        let got: io::Result<()> = retry(
            "the guard pipe",
            Duration::from_millis(100),
            Duration::from_millis(30),
            || {
                tries += 1;
                Err(io::Error::other(format!("access denied ({tries})")))
            },
        );
        let took = t.elapsed();
        let why = got.unwrap_err().to_string();
        assert_eq!(why, format!("access denied ({tries})"));
        assert!(tries >= 2, "{tries}");
        assert!(took >= Duration::from_millis(100), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[test]
    fn retry_gives_up_at_the_limit_and_does_not_loop_forever() {
        // A bounded, deterministic catch for the match-guard -> `true` mutant of
        // `Err(e) if start.elapsed() < limit` (pipe.rs:128). The real code
        // retries only while within the limit and then returns the last error;
        // the mutant retries forever on a call that keeps failing. Run it on a
        // thread and require it to end within a bound: the original returns Err
        // just after the limit; the mutant never returns, so this fails its
        // assertion in 3 s (a clean FAIL) instead of hanging until nextest's
        // slow-timeout (#23, and it runs first under `priority = 100`).
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let got: io::Result<()> = retry(
                "the guard pipe",
                Duration::from_millis(20),
                Duration::from_millis(5),
                || Err(io::Error::other("access denied")),
            );
            let _ = done_tx.send(got.is_err());
        });
        match done_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(was_err) => assert!(was_err, "retry returned Ok although the call always failed"),
            Err(_) => panic!("retry looped past its limit instead of giving up within 3 s"),
        }
    }

    #[test]
    fn a_request_while_switching_is_refused_at_the_pipe() {
        let (s, _rx) = served();
        s.shared.update(|v| v.running = Some(Mode::Dev));
        let busy = call(
            &s.name,
            &Request::Dev {
                build: None,
                force: false,
                dry_run: false,
            },
        )
        .unwrap();
        assert!(!busy.ok);
        assert_eq!(busy.detail, "busy");
        let job = call(&s.name, &Request::JobBegin { run: 7 }).unwrap();
        assert_eq!((job.ok, job.detail.as_str()), (false, "switching"));
    }

    #[test]
    fn event_preempts_the_switch_and_answers_when_it_ended() {
        let (s, _rx) = served();
        s.shared.update(|v| v.running = Some(Mode::Dev));
        let shared = Arc::clone(&s.shared);
        let cancel = s.cancel.clone();
        let ender = thread::spawn(move || {
            let t = Instant::now();
            while !cancel.preempted() {
                assert!(t.elapsed() < Duration::from_secs(5), "never pre-empted");
                thread::sleep(Duration::from_millis(20));
            }
            thread::sleep(Duration::from_millis(200));
            shared.update(|v| {
                v.running = None;
                v.mode = Mode::Event;
                v.last = Some(Outcome::Done);
            });
        });
        let t = Instant::now();
        let reply = call(&s.name, &Request::Event { dry_run: false }).unwrap();
        ender.join().unwrap();
        assert!(t.elapsed() >= Duration::from_millis(200));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.mode, Mode::Event);
        assert_eq!(
            reply.detail,
            "pre-empted the switch in progress; event: done"
        );
    }

    /// The state frames of a subscription, as the tray reads them.
    fn state(update: Update) -> Reply {
        match update {
            Update::State(reply) => reply,
            Update::Quit => panic!("a quit, not a state"),
        }
    }

    /// A tray's subscription on `s`, once the guard counts it. Its reads
    /// end after 5 s instead of hanging on a frame that never comes (Unix;
    /// Windows pipes have no timeout).
    fn subscribed(s: &Served) -> Stream {
        let stream = Stream::connect(pipe_name(&s.name).unwrap()).unwrap();
        #[cfg(unix)]
        stream
            .set_recv_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut wire = &stream;
        proto::write_frame(&mut wire, &Request::Subscribe).unwrap();
        let first = state(proto::read_update(&mut wire).unwrap());
        assert!(first.ok);
        let t = Instant::now();
        while s.shared.view().subscribers == 0 {
            assert!(t.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(10));
        }
        stream
    }

    /// The tray's subscription (S6 plan Task 11): state frames, and the
    /// guard's quit as `Update::Quit` (`{"cmd":"quit"}`), the frame the tray
    /// exits on.
    #[test]
    fn a_subscriber_gets_changes_and_the_trays_quit() {
        let (s, _rx) = served();
        let stream = subscribed(&s);
        let mut wire = &stream;
        let t = Instant::now();
        s.shared.update(|v| v.status = "mode dev".into());
        let change = state(proto::read_update(&mut wire).unwrap());
        assert_eq!(change.detail, "mode dev");
        // Well inside the subscriber's own poll: the change woke it.
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        let t = Instant::now();
        s.shared.tray_quit().unwrap();
        assert_eq!(proto::read_update(&mut wire).unwrap(), Update::Quit);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // A quit alone sends no status: the next frame is the next change.
        s.shared.update(|v| v.status = "mode event".into());
        let next = state(proto::read_update(&mut wire).unwrap());
        assert_eq!(next.detail, "mode event");
        drop(stream);
        // The subscriber leaves at its next write.
        let t = Instant::now();
        while s.shared.view().subscribers > 0 {
            s.shared.update(|_| {});
            assert!(t.elapsed() < Duration::from_secs(5), "never left");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The guard stops the tray while the tray waits to connect again (its
    /// 2 s retry): the quit waits for the next subscription, which gets it
    /// right after its first state.
    #[test]
    fn a_tray_that_subscribes_after_the_quit_gets_it_at_once() {
        let (s, _rx) = served();
        assert_eq!(s.shared.tray_quit(), Ok(()));
        let stream = subscribed(&s);
        let mut wire = &stream;
        assert_eq!(proto::read_update(&mut wire).unwrap(), Update::Quit);
        // Taken once: another subscription gets states only.
        drop(stream);
        let again = subscribed(&s);
        let mut wire = &again;
        s.shared.update(|v| v.status = "mode dev".into());
        let next = state(proto::read_update(&mut wire).unwrap());
        assert_eq!(next.detail, "mode dev");
    }

    /// A tray that stays connected but stops reading is dropped within a
    /// bound: a write to it gives up after a while, so no client holds a
    /// guard thread in a write. Each state here is larger than a Windows
    /// pipe's buffer (512 bytes); a Unix socket's fills after a few dozen.
    #[test]
    fn a_subscriber_that_does_not_read_is_dropped_within_a_bound() {
        let (s, _rx) = served();
        let stream = subscribed(&s);
        let big = "x".repeat(4000);
        let t = Instant::now();
        let mut n = 0u32;
        while s.shared.view().subscribers > 0 {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "a subscriber that does not read held its guard thread"
            );
            n += 1;
            s.shared.update(|v| v.status = format!("{n} {big}"));
            thread::sleep(Duration::from_millis(10));
        }
        drop(stream);
    }

    /// The guard closes a connection as it drops it, even while its client
    /// has not read the reply the guard wrote last, so a client that never
    /// reads holds up no later close. interprocess's flush on drop (limbo)
    /// would keep the guard's end open on the process's one linger thread
    /// until that client had read everything, and every connection the
    /// guard dropped after it would wait there behind it, unclosed: a leak
    /// in a process that runs for months (the engine's pipes met it in
    /// Windows CI run 36373563262). The client still reads the reply, then
    /// the end.
    #[cfg(windows)]
    #[test]
    fn a_client_that_does_not_read_holds_up_no_close() {
        use iem_win::pipe::write_within;

        let (s, _rx) = served();
        // Refused at its first frame: a short reply that fits the pipe,
        // then the guard drops the connection. Nobody reads that reply yet.
        let mute = refused(&s);
        // Once the guard's end is closed, a byte the client writes finds
        // the pipe closing; while it stays open, each byte waits in the
        // pipe (a few dozen fit its 512 bytes).
        let gone = {
            let start = Instant::now();
            loop {
                match write_within(end(&mute), &[0], Duration::from_millis(100)) {
                    Err(gone) => break Some(gone),
                    Ok(_) if start.elapsed() < Duration::from_secs(2) => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Ok(_) => break None,
                }
            }
        };
        // Meanwhile a later connection the guard drops the same way: its
        // client reads the reply, then the end.
        let later = refused(&s);
        let later_reply = reply_of(&later);
        let later_closed = ends(&later, Duration::from_secs(2));
        // The silent client reads the reply the guard wrote before it
        // closed, then the end (reading it also frees whatever waited for
        // it, before any assertion below can fail).
        let late_reply = reply_of(&mute);
        let late_closed = ends(&mute, Duration::from_secs(5));
        for reply in [&later_reply, &late_reply] {
            assert!(!reply.ok);
            assert!(
                reply.detail.starts_with("bad request: "),
                "{}",
                reply.detail
            );
        }
        assert!(late_closed, "the silent client reads the end");
        let Some(gone) = gone else {
            panic!("the guard kept a dropped connection open until its client read it");
        };
        assert!(
            matches!(gone.raw_os_error(), Some(BROKEN | CLOSING | NOT_CONNECTED)),
            "{gone}"
        );
        assert!(
            later_closed,
            "a close waited for a client that does not read"
        );
    }

    /// `ERROR_BROKEN_PIPE`.
    #[cfg(windows)]
    const BROKEN: i32 = 109;
    /// `ERROR_NO_DATA`: the pipe is being closed.
    #[cfg(windows)]
    const CLOSING: i32 = 232;
    /// `ERROR_PIPE_NOT_CONNECTED`.
    #[cfg(windows)]
    const NOT_CONNECTED: i32 = 233;

    /// The client's end of `stream`.
    #[cfg(windows)]
    fn end(stream: &Stream) -> std::os::windows::io::BorrowedHandle<'_> {
        use std::os::windows::io::AsHandle;
        let Stream::NamedPipe(pipe) = stream;
        pipe.inner().as_handle()
    }

    /// A connection whose first frame is no request (`{}`): the guard
    /// answers "bad request" and drops it. Returns once that reply waits in
    /// the pipe. The frame goes out through `write_within`, which leaves
    /// this end out of the flush on drop, so it never waits in limbo itself.
    #[cfg(windows)]
    fn refused(s: &Served) -> Stream {
        let stream = Stream::connect(pipe_name(&s.name).unwrap()).unwrap();
        let body = b"{}";
        let mut frame = (body.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(body);
        let wrote =
            iem_win::pipe::write_within(end(&stream), &frame, Duration::from_secs(5)).unwrap();
        assert_eq!(wrote, frame.len());
        let t = Instant::now();
        while iem_win::pipe::available(end(&stream)).unwrap() == 0 {
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "no reply to a bad request"
            );
            thread::sleep(Duration::from_millis(10));
        }
        stream
    }

    /// The reply that waits in the pipe.
    #[cfg(windows)]
    fn reply_of(stream: &Stream) -> Reply {
        let mut wire = stream;
        proto::read_msg(&mut wire).unwrap()
    }

    /// Whether the guard's end of `stream` is closed within `limit`, once
    /// everything it wrote was read: a peek then fails.
    #[cfg(windows)]
    fn ends(stream: &Stream, limit: Duration) -> bool {
        let t = Instant::now();
        loop {
            match iem_win::pipe::available(end(stream)) {
                Err(e) => {
                    assert!(
                        matches!(e.raw_os_error(), Some(BROKEN | CLOSING | NOT_CONNECTED)),
                        "{e}"
                    );
                    return true;
                }
                Ok(_) if t.elapsed() < limit => thread::sleep(Duration::from_millis(10)),
                Ok(_) => return false,
            }
        }
    }

    #[test]
    fn garbage_is_answered_and_the_connection_closed() {
        let (s, _rx) = served();
        let stream = Stream::connect(pipe_name(&s.name).unwrap()).unwrap();
        let mut wire = &stream;
        let body = br#"{"cmd":"reboot"}"#;
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(body);
        std::io::Write::write_all(&mut wire, &bytes).unwrap();
        let reply: Reply = proto::read_msg(&mut wire).unwrap();
        assert!(!reply.ok);
        assert!(
            reply.detail.starts_with("bad request: "),
            "{}",
            reply.detail
        );
        assert!(matches!(
            proto::read_frame(&mut wire),
            Err(FrameError::Closed | FrameError::Io(_))
        ));
    }

    #[test]
    fn a_guard_that_stopped_takes_no_request() {
        let (s, rx) = served();
        drop(rx);
        let reply = call(&s.name, &Request::Quit).unwrap();
        assert!(!reply.ok);
        assert_eq!(reply.detail, "the guard is stopping");
        let (t, rx) = served();
        let dropper = thread::spawn(move || {
            let job = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(job);
        });
        let reply = call(&t.name, &Request::Quit).unwrap();
        dropper.join().unwrap();
        assert_eq!(reply.detail, "the guard dropped the request");
    }

    /// The first-instance flag: while a guard listens, a second guard or a
    /// squatter never gets the pipe's name; once that listener has gone
    /// (the old guard's process ended after a hand-over) the name is free.
    #[cfg(windows)]
    #[test]
    fn a_held_guard_pipe_name_is_refused_until_its_listener_has_gone() {
        const ERROR_ACCESS_DENIED: i32 = 5;
        let dir = tempfile::tempdir().unwrap();
        let name = test_name(dir.path());
        let first = listen(&name).unwrap();
        let second = listen(&name).unwrap_err();
        assert_eq!(second.raw_os_error(), Some(ERROR_ACCESS_DENIED), "{second}");
        drop(first);
        let again = listen(&name);
        assert!(again.is_ok(), "{:?}", again.err());
    }

    #[test]
    fn nothing_on_the_pipe_is_a_connect_error() {
        let dir = tempfile::tempdir().unwrap();
        let name = test_name(dir.path());
        match call(&name, &Request::Status) {
            Err(e @ CallError::Connect(_)) => {
                assert!(e.to_string().starts_with("no guard on the pipe: "), "{e}");
            }
            other => panic!("{other:?}"),
        }
        let broke = CallError::Pipe(FrameError::Closed);
        assert_eq!(broke.to_string(), "the guard pipe broke: guard pipe closed");
    }
}
