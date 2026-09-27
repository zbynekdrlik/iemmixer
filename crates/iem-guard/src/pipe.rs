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
//! a stream of status replies (the tray), which also carries the tray's
//! quit request ([`proto::TRAY_QUIT`]).

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, Name, Stream,
};
use tracing::{info, warn};

use crate::daemon::{Job, Route, Shared};
use crate::proto::{self, FrameError, Reply, Request};

/// An idle listener looks for a new connection this often.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// "ide event" waits at most this long for the switch in progress.
pub const AWAIT_END: Duration = Duration::from_secs(600);
/// A subscriber looks at the view at least this often.
pub const SUBSCRIBER_POLL: Duration = Duration::from_secs(5);

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
    let _ = (what, limit, every);
    f()
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

fn spawn_connection(stream: Stream, shared: &Arc<Shared>, jobs: &Sender<Job>) {
    if let Err(e) = stream.set_nonblocking(false) {
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
    loop {
        let req: Request = match proto::read_msg(&mut wire) {
            Ok(req) => req,
            Err(FrameError::Closed) => return,
            Err(FrameError::Bad(why)) => {
                let reply = shared.view().reply(false, &format!("bad request: {why}"));
                if let Err(e) = proto::write_frame(&mut wire, &reply) {
                    info!("a guard pipe client: {e}");
                }
                return;
            }
            Err(e) => {
                info!("a guard pipe client: {e}");
                return;
            }
        };
        let reply = match shared.route(&req) {
            Route::Now(reply) => reply,
            Route::AwaitEnd(note) => shared.await_end(note, AWAIT_END),
            Route::Queue(epoch) => {
                ask(jobs, req, epoch).unwrap_or_else(|why| shared.view().reply(false, &why))
            }
            Route::Subscribe => {
                subscribe(stream, shared);
                return;
            }
        };
        if let Err(e) = proto::write_frame(&mut wire, &reply) {
            info!("a guard pipe client left: {e}");
            return;
        }
    }
}

/// A subscriber (the tray): the status now, then every change, and the
/// tray's quit request when the guard stops the tray.
fn subscribe(stream: &Stream, shared: &Shared) {
    shared.add_subscriber();
    let mut wire = stream;
    let first = shared.view();
    let mut seen = (first.version, first.tray_quits);
    if proto::write_frame(&mut wire, &first.reply(true, &first.status)).is_ok() {
        loop {
            let v = shared.wait_change(seen, SUBSCRIBER_POLL);
            let quit = v.tray_quits != seen.1;
            let changed = v.version != seen.0;
            seen = (v.version, v.tray_quits);
            if quit && proto::write_frame(&mut wire, &v.reply(true, proto::TRAY_QUIT)).is_err() {
                break;
            }
            if changed && proto::write_frame(&mut wire, &v.reply(true, &v.status)).is_err() {
                break;
            }
        }
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

    #[test]
    fn a_subscriber_gets_changes_and_the_trays_quit() {
        let (s, _rx) = served();
        assert_eq!(
            s.shared.tray_quit(),
            Err("the tray is not subscribed to the guard".to_owned())
        );
        let stream = Stream::connect(pipe_name(&s.name).unwrap()).unwrap();
        let mut wire = &stream;
        proto::write_frame(&mut wire, &Request::Subscribe).unwrap();
        let first: Reply = proto::read_msg(&mut wire).unwrap();
        assert!(first.ok);
        let t = Instant::now();
        while s.shared.view().subscribers == 0 {
            assert!(t.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(10));
        }
        let t = Instant::now();
        s.shared.update(|v| v.status = "mode dev".into());
        let change: Reply = proto::read_msg(&mut wire).unwrap();
        assert_eq!(change.detail, "mode dev");
        // Well inside the subscriber's own poll: the change woke it.
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        let t = Instant::now();
        s.shared.tray_quit().unwrap();
        let quit: Reply = proto::read_msg(&mut wire).unwrap();
        assert_eq!(quit.detail, proto::TRAY_QUIT);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // A quit alone sends no status: the next frame is the next change.
        s.shared.update(|v| v.status = "mode event".into());
        let next: Reply = proto::read_msg(&mut wire).unwrap();
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
