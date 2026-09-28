//! The engine through its pipes (design note §3.6, §3.7): an in-process
//! engine on NullRt at B = 32 with a temporary state directory, and the
//! `iem-engine` binary. Every read has a 5 s timeout; every wait is bounded.
//!
//! Unix sockets on Linux, named pipes on Windows (the `windows` CI job, S6).
//! Windows pipes have no timeouts, so a client reads the way the engine does
//! (`pipe::read_loop` on a thread of its own, closed when the client is
//! dropped): it never leaves the engine blocked on a full pipe, and every
//! wait on it is a bounded channel receive.

mod common;

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::{InputSignal, Planar, wav};
use iem_engine::control::Exit;
use iem_engine::core::Flags;
use iem_engine::engine::{EngineError, RunConfig, run};
use iem_engine::pipe::{Conn, Framer, control_name, media_name, read_loop};
use iem_engine_proto::media::stream;
use iem_engine_proto::{
    AlarmCode, Change, ClientMsg, Cmd, EngineMsg, ErrCode, FRAME_48K, FrameError, Hello, InputId,
    MAX_FRAME, MediaHeader, MixId, PROTO, Reply, Role, Source, TopologyInfo, write_frame,
    write_media,
};
use interprocess::local_socket::Stream;
use interprocess::local_socket::prelude::*;

const WAIT: Duration = Duration::from_secs(5);
static NEXT: AtomicU64 = AtomicU64::new(0);

fn pipe_name(dir: &tempfile::TempDir) -> String {
    if cfg!(unix) {
        dir.path().join("ctl.sock").to_string_lossy().into_owned()
    } else {
        format!(
            "iem-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }
}

fn connect(
    name: impl Fn() -> std::io::Result<interprocess::local_socket::Name<'static>>,
) -> Stream {
    let start = Instant::now();
    loop {
        match Stream::connect(name().unwrap()) {
            Ok(s) => return s,
            Err(e) if start.elapsed() < WAIT => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("cannot connect: {e}"),
        }
    }
}

/// A connection read on a thread of its own through the engine's reader
/// (`read_loop`): what `next` cuts arrives on `rx`, then why the reading
/// stopped. Dropping it closes the connection like the engine does.
struct Reader<T> {
    conn: Conn,
    rx: mpsc::Receiver<Result<T, FrameError>>,
}

impl<T: Send + 'static> Reader<T> {
    fn start(stream: Stream, next: fn(&mut Framer) -> Result<Option<T>, FrameError>) -> Self {
        let conn = Conn::new(stream);
        let (tx, rx) = mpsc::channel();
        let reader = conn.clone();
        std::thread::spawn(move || {
            let why = read_loop(&reader, next, |item| tx.send(Ok(item)).is_ok());
            let _ = tx.send(Err(why));
        });
        Self { conn, rx }
    }

    /// The next item, or why there is none: `Closed` once the reader
    /// stopped, `TimedOut` after 5 s.
    fn try_recv(&self) -> Result<T, FrameError> {
        match self.rx.recv_timeout(WAIT) {
            Ok(item) => item,
            Err(RecvTimeoutError::Timeout) => {
                Err(FrameError::Io(std::io::ErrorKind::TimedOut.into()))
            }
            Err(RecvTimeoutError::Disconnected) => Err(FrameError::Closed),
        }
    }

    fn write(&self, bytes: &[u8]) {
        (&*self.conn.stream).write_all(bytes).unwrap();
    }
}

impl<T> Drop for Reader<T> {
    fn drop(&mut self) {
        self.conn.close();
    }
}

struct Client {
    r: Reader<Vec<u8>>,
}

impl Client {
    fn new(pipe: &str) -> Self {
        let p = pipe.to_owned();
        Self {
            r: Reader::start(connect(move || control_name(&p)), Framer::next_frame),
        }
    }

    fn send(&mut self, msg: &ClientMsg) {
        write_frame(&mut &*self.r.conn.stream, msg).unwrap();
    }

    /// Raw bytes on the control pipe.
    fn write_raw(&mut self, bytes: &[u8]) {
        self.r.write(bytes);
    }

    fn try_recv(&mut self) -> Result<EngineMsg, FrameError> {
        let bytes = self.r.try_recv()?;
        Ok(serde_json::from_slice(&bytes).unwrap())
    }

    fn recv(&mut self) -> EngineMsg {
        self.try_recv().unwrap()
    }

    /// The next message `pick` accepts, skipping the rest (meters, status…).
    fn wait<T>(&mut self, mut pick: impl FnMut(&EngineMsg) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < WAIT, "timed out waiting");
            if let Some(v) = pick(&self.recv()) {
                return v;
            }
        }
    }

    fn hello(&mut self, role: Role) -> (Hello, TopologyInfo, u64) {
        self.send(&ClientMsg::Hello {
            proto: PROTO,
            role,
            client: "test".into(),
        });
        let EngineMsg::Hello(h) = self.recv() else {
            panic!("hello first")
        };
        let EngineMsg::Topology(t) = self.recv() else {
            panic!("topology second")
        };
        let EngineMsg::State { rev, .. } = self.recv() else {
            panic!("state third")
        };
        (h, t, rev)
    }

    fn request(&mut self, id: u64, cmd: Cmd) -> Reply {
        self.send(&ClientMsg::Request {
            id,
            origin: Some(1000 + id),
            cmd,
        });
        self.wait(|m| match m {
            EngineMsg::Reply(r) if r.id == id => Some(r.clone()),
            _ => None,
        })
    }

    /// True when the engine closed the connection.
    fn closed(&mut self) -> bool {
        let start = Instant::now();
        while start.elapsed() < WAIT {
            match self.try_recv() {
                Ok(_) => {}
                Err(FrameError::Closed) => return true,
                Err(FrameError::Io(e)) => return e.kind() != std::io::ErrorKind::TimedOut,
                Err(FrameError::TooLarge(_)) => return false,
            }
        }
        false
    }
}

struct Engine {
    pipe: String,
    dir: tempfile::TempDir,
    handle: Option<JoinHandle<Result<Exit, EngineError>>>,
}

impl Engine {
    fn start(flags: Flags, signal: InputSignal) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), flags, signal)
    }

    fn start_in(dir: tempfile::TempDir, flags: Flags, signal: InputSignal) -> Self {
        Self::start_with(dir, |cfg| {
            cfg.flags = flags;
            cfg.signal = signal;
        })
    }

    /// An engine with the test's changes to the default configuration.
    fn start_with(dir: tempfile::TempDir, edit: impl FnOnce(&mut RunConfig)) -> Self {
        let pipe = pipe_name(&dir);
        let mut cfg = RunConfig::new(common::site_path(), dir.path().join("state"), pipe.clone());
        cfg.solo_grace = Duration::from_millis(400);
        edit(&mut cfg);
        let handle = std::thread::spawn(move || run(cfg));
        Self {
            pipe,
            dir,
            handle: Some(handle),
        }
    }

    fn client(&self) -> Client {
        Client::new(&self.pipe)
    }

    /// Waits for `run` to return.
    fn exit(&mut self) -> Exit {
        let handle = self.handle.take().unwrap();
        let start = Instant::now();
        while !handle.is_finished() {
            assert!(start.elapsed() < WAIT, "the engine did not stop");
            std::thread::sleep(Duration::from_millis(10));
        }
        handle.join().unwrap().unwrap()
    }

    fn shutdown(mut self) -> tempfile::TempDir {
        let mut c = self.client();
        c.hello(Role::Control);
        let r = c.request(99, Cmd::Shutdown);
        assert!(r.error.is_none());
        assert_eq!(self.exit(), Exit::Shutdown { faded: true });
        self.dir
    }
}

fn set_mix(mix: &str, volume_db: f64) -> Cmd {
    Cmd::SetMix {
        mix: MixId::new(mix),
        volume_db: Some(volume_db),
        muted: None,
    }
}

#[test]
fn hello_topology_state_then_reply_and_delta() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut ctl = e.client();
    let (h, topo, rev) = ctl.hello(Role::Control);
    assert_eq!(
        (h.proto, h.sample_rate, h.block, h.role),
        (PROTO, 96_000, 32, Role::Control)
    );
    assert_eq!(h.state_rev, 0);
    assert_eq!(rev, 0);
    assert_eq!(topo.hash, h.topology_hash);
    assert_eq!(
        (topo.inputs.len(), topo.groups.len(), topo.mixes.len()),
        (24, 1, 11)
    );
    assert!(h.engine_build.starts_with(env!("CARGO_PKG_VERSION")));
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let reply = ctl.request(7, set_mix("member1", -3.0));
    assert_eq!((reply.rev, reply.error), (1, None));
    for c in [&mut ctl, &mut obs] {
        let (rev, origin, changes) = c.wait(|m| match m {
            EngineMsg::Delta {
                rev,
                origin,
                changes,
            } => Some((*rev, *origin, changes.clone())),
            _ => None,
        });
        assert_eq!((rev, origin), (1, Some(1007)));
        assert!(
            matches!(&changes[..], [Change::MixOut { mix, out }] if mix.0 == "member1" && out.volume_db == -3.0)
        );
    }
    // GetState answers with the state; GetTopology with the topology.
    let r = obs.request(8, Cmd::GetState);
    assert!(r.error.is_none());
    let state = obs.wait(|m| match m {
        EngineMsg::State { rev, state, .. } => Some((*rev, state.clone())),
        _ => None,
    });
    assert_eq!(state.0, 1);
    assert_eq!(state.1.mixes[&MixId::new("member1")].out.volume_db, -3.0);
    obs.request(9, Cmd::GetTopology);
    obs.wait(|m| matches!(m, EngineMsg::Topology(_)).then_some(()));
    e.shutdown();
}

#[test]
fn observers_cannot_write_and_strangers_must_say_hello() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut stranger = e.client();
    let r = stranger.request(1, Cmd::Ping);
    assert_eq!(r.error.unwrap().code, ErrCode::BadRequest);
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let r = obs.request(2, set_mix("member2", -1.0));
    assert_eq!(r.error.unwrap().code, ErrCode::NotController);
    assert!(obs.request(3, Cmd::Ping).error.is_none());
    // A client far older than the engine is refused and closed.
    let mut old = e.client();
    old.send(&ClientMsg::Hello {
        proto: PROTO + 5,
        role: Role::Observe,
        client: "new".into(),
    });
    assert!(
        matches!(old.recv(), EngineMsg::Hello(h) if h.proto == PROTO),
        "a newer client is served at N"
    );
    let mut ancient = e.client();
    ancient.send(&ClientMsg::Hello {
        proto: 0,
        role: Role::Observe,
        client: "ancient".into(),
    });
    let r = ancient.wait(|m| match m {
        EngineMsg::Reply(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!(r.error.unwrap().code, ErrCode::Unsupported);
    assert!(ancient.closed());
    e.shutdown();
}

#[test]
fn a_new_controller_supersedes_the_old() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut first = e.client();
    first.hello(Role::Control);
    let mut second = e.client();
    second.hello(Role::Control);
    first.wait(|m| matches!(m, EngineMsg::Superseded).then_some(()));
    assert!(first.closed());
    assert!(second.request(1, set_mix("member3", -2.0)).error.is_none());
    e.shutdown();
}

#[test]
fn garbage_gets_a_typed_error_and_oversize_closes_only_that_connection() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = e.client();
    c.hello(Role::Control);
    let mut raw = 3u32.to_le_bytes().to_vec();
    raw.extend_from_slice(b"xyz");
    c.write_raw(&raw);
    let r = c.wait(|m| match m {
        EngineMsg::Reply(r) => Some(r.clone()),
        _ => None,
    });
    assert_eq!((r.id, r.error.unwrap().code), (0, ErrCode::BadRequest));
    c.send(&ClientMsg::Request {
        id: 5,
        origin: None,
        cmd: Cmd::Ping,
    });
    assert!(
        c.request(6, Cmd::Ping).error.is_none(),
        "the connection survives"
    );
    let mut bad = e.client();
    bad.hello(Role::Observe);
    bad.write_raw(&((MAX_FRAME + 1) as u32).to_le_bytes());
    assert!(bad.closed());
    assert!(
        c.request(7, Cmd::Ping).error.is_none(),
        "others are unaffected"
    );
    e.shutdown();
}

#[test]
fn meters_and_status_flow() {
    let e = Engine::start(
        Flags::default(),
        InputSignal::Sine {
            hz: 1000.0,
            amp: 0.1,
        },
    );
    let mut c = e.client();
    let (_, topo, _) = c.hello(Role::Control);
    // mic1 at +24 dB trim into member1 at +12 dB drives its −6 dB limiter.
    let hot = Cmd::SetInput {
        input: InputId::new("mic1"),
        trim_db: Some(24.0),
        muted: None,
        processing: None,
    };
    assert!(c.request(1, hot).error.is_none());
    let level = Cmd::SetLevel {
        mix: MixId::new("member1"),
        source: Source::Input(InputId::new("mic1")),
        gain_db: Some(12.0),
        pan: None,
        muted: None,
    };
    assert!(c.request(2, level).error.is_none());
    let m1 = topo.mixes.iter().position(|m| m.id.0 == "member1").unwrap();
    let start = Instant::now();
    let mut meters = 0;
    let mut loud = false;
    let mut statuses = Vec::new();
    let mut active = 0.0;
    let mut gr = 0.0f32;
    while start.elapsed() < Duration::from_millis(2500) {
        match c.recv() {
            EngineMsg::Meters(m) => {
                meters += 1;
                assert_eq!(
                    (m.inputs.len(), m.mixes.len(), m.groups.len(), m.gr_db.len()),
                    (24, 11, 11, 11)
                );
                assert_eq!(m.limiter_active_s.len(), 11);
                loud |= m.inputs.iter().skip(1).all(|p| p[0] > 0.05);
                active = m.limiter_active_s[m1];
                gr = gr.min(m.gr_db[m1]);
            }
            EngineMsg::Status(s) => statuses.push(s),
            _ => {}
        }
    }
    assert!(meters >= 3, "{meters} meter frames");
    assert!(loud, "the sine shows on every input meter");
    assert!(gr < -3.0, "member1's limiter works: {gr} dB");
    assert!(
        active > 0.0 && active < 5.0,
        "limiter-active seconds {active}"
    );
    assert!(
        (1..=4).contains(&statuses.len()),
        "about one status a second: {}",
        statuses.len()
    );
    let s = statuses.last().unwrap();
    assert!(s.callbacks > 0 && !s.faulted);
    assert!(
        s.process_max_us > 0.0 && s.process_max_us < 100_000.0,
        "{}",
        s.process_max_us
    );
    assert_eq!(s.cmd_backlog, 0);
    e.shutdown();
}

#[test]
fn a_test_signal_ends_after_its_ttl() {
    let e = Engine::start(
        Flags {
            test_signal: true,
            fault_injection: false,
        },
        InputSignal::Silence,
    );
    let mut c = e.client();
    c.hello(Role::Control);
    let start = Instant::now();
    let r = c.request(
        1,
        Cmd::StartTestSignal {
            input: InputId::new("mic1"),
            hz: 1000.0,
            dbfs: -30.0,
            ttl_s: 0.4,
        },
    );
    assert!(r.error.is_none());
    let signal = |m: &EngineMsg| match m {
        EngineMsg::Delta { changes, .. } => changes.iter().find_map(|ch| match ch {
            Change::TestSignal { signal } => Some(signal.clone()),
            _ => None,
        }),
        _ => None,
    };
    assert!(c.wait(signal).is_some());
    assert!(c.wait(signal).is_none());
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(350) && took < Duration::from_secs(3),
        "{took:?}"
    );
    e.shutdown();
}

#[test]
fn a_huge_talkback_frame_trips_the_sanitiser_alarm() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = e.client();
    c.hello(Role::Observe);
    let m = media_client(&e.pipe);
    m.send_media(
        &MediaHeader {
            stream: stream::TALKBACK,
            channels: 1,
            seq: 0,
            frames: FRAME_48K as u16,
        },
        // A whole 20 ms frame: shorter bursts are underruns and never open
        // the talkback gate.
        &[1e30; FRAME_48K],
    );
    let detail = c.wait(|msg| match msg {
        EngineMsg::Alarm(a) if a.code == AlarmCode::Sanitizer => Some(a.detail.clone()),
        _ => None,
    });
    assert!(detail.contains("trips"), "{detail}");
    // New connections get the alarm replayed after their hello.
    let mut late = e.client();
    late.hello(Role::Observe);
    late.wait(|msg| {
        matches!(msg, EngineMsg::Alarm(a) if a.code == AlarmCode::Sanitizer).then_some(())
    });
    drop(m);
    e.shutdown();
}

#[test]
fn a_ninth_connection_is_refused() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut clients: Vec<Client> = (0..8).map(|_| e.client()).collect();
    for c in &mut clients {
        assert!(
            c.request(1, Cmd::Ping).error.is_some(),
            "no hello yet: refused but connected"
        );
    }
    let mut ninth = e.client();
    assert!(ninth.closed());
    drop(clients);
    // Let the engine see the eight closes before the shutdown client connects.
    std::thread::sleep(Duration::from_millis(500));
    e.shutdown();
}

/// A media connection: listen frames in, talkback frames out.
type Media = Reader<(MediaHeader, Vec<f32>)>;

fn media_client(pipe: &str) -> Media {
    let p = pipe.to_owned();
    Reader::start(connect(move || media_name(&p)), Framer::next_media)
}

impl Media {
    fn send_media(&self, h: &MediaHeader, samples: &[f32]) {
        write_media(&mut &*self.conn.stream, h, samples).unwrap();
    }
}

#[test]
fn listen_frames_arrive_on_the_media_pipe() {
    let e = Engine::start(
        Flags::default(),
        InputSignal::Sine {
            hz: 1000.0,
            amp: 0.1,
        },
    );
    let mut c = e.client();
    c.hello(Role::Control);
    assert!(
        c.request(
            1,
            Cmd::StartListen {
                mix: MixId::new("engineer")
            }
        )
        .error
        .is_none()
    );
    assert!(
        c.request(
            2,
            Cmd::StartListen {
                mix: MixId::new("member4")
            }
        )
        .error
        .is_none()
    );
    let r = c.request(
        3,
        Cmd::StartListen {
            mix: MixId::new("member5"),
        },
    );
    assert_eq!(r.error.unwrap().code, ErrCode::NoSource);
    let m = media_client(&e.pipe);
    let mut seen = [0u64; 2];
    let start = Instant::now();
    while (seen[0] < 3 || seen[1] < 3) && start.elapsed() < WAIT {
        let (h, samples) = m.try_recv().unwrap();
        assert_eq!((h.channels, usize::from(h.frames)), (2, FRAME_48K));
        assert_eq!(samples.len(), 2 * FRAME_48K);
        let slot = usize::from(h.stream);
        assert!(slot < 2);
        seen[slot] += 1;
    }
    assert!(seen[0] >= 3 && seen[1] >= 3, "{seen:?}");
    e.shutdown();
}

#[test]
fn talkback_frames_reach_the_talkback_input() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = e.client();
    let (_, topo, _) = c.hello(Role::Observe);
    let tb = topo.inputs.iter().position(|i| i.talkback).unwrap();
    let m = media_client(&e.pipe);
    let frame = vec![0.5f32; FRAME_48K];
    let start = Instant::now();
    let mut heard = false;
    let mut seq = 0;
    while !heard && start.elapsed() < WAIT {
        m.send_media(
            &MediaHeader {
                stream: stream::TALKBACK,
                channels: 1,
                seq,
                frames: FRAME_48K as u16,
            },
            &frame,
        );
        seq += 1;
        let deadline = Instant::now() + Duration::from_millis(20);
        while Instant::now() < deadline {
            if let EngineMsg::Meters(mm) = c.recv() {
                heard |= mm.inputs[tb][0] > 0.1;
            }
        }
    }
    assert!(heard, "0.5 talkback shows as ≈0.19 on the talkback input");
    // Without frames the gate closes.
    let quiet = Instant::now();
    let mut silent = false;
    while !silent && quiet.elapsed() < WAIT {
        if let EngineMsg::Meters(mm) = c.recv() {
            silent = quiet.elapsed() > Duration::from_millis(300) && mm.inputs[tb][0] == 0.0;
        }
    }
    assert!(silent);
    drop(m);
    e.shutdown();
}

#[test]
fn solos_survive_a_quick_reconnect_and_clear_after_the_grace() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let solo = Cmd::SetSolo {
        mix: MixId::new("member3"),
        sources: vec![Source::Input(InputId::new("mic2"))],
    };
    let transient = |c: &mut Client| {
        c.request(50, Cmd::GetState);
        c.wait(|m| match m {
            EngineMsg::State { transient, .. } => Some(transient.clone()),
            _ => None,
        })
    };
    {
        let mut c = e.client();
        c.hello(Role::Control);
        assert!(c.request(1, solo).error.is_none());
    }
    let mut back = e.client();
    back.hello(Role::Control);
    assert_eq!(
        transient(&mut back).solo.len(),
        1,
        "kept across a quick reconnect"
    );
    drop(back);
    std::thread::sleep(Duration::from_millis(900));
    let mut later = e.client();
    later.hello(Role::Control);
    assert!(
        transient(&mut later).solo.is_empty(),
        "cleared after the grace"
    );
    e.shutdown();
}

fn h_hash(_e: &Engine) -> String {
    iem_engine::topology::compile(&iem_engine::site::load(&common::site_path()).unwrap())
        .unwrap()
        .hash
}

#[test]
fn shutdown_saves_fades_and_releases() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = e.client();
    let (_, _, _) = c.hello(Role::Control);
    assert_eq!(
        c.wait(|m| match m {
            EngineMsg::Alarm(a) => Some(a.code),
            _ => None,
        }),
        AlarmCode::StateLost,
        "a fresh state directory has no state"
    );
    assert!(c.request(1, set_mix("member6", -4.0)).error.is_none());
    let mut e = e;
    let r = c.request(2, Cmd::Shutdown);
    assert!(r.error.is_none());
    c.wait(|m| {
        matches!(m, EngineMsg::DriverReleased { reason } if reason == "shutdown").then_some(())
    });
    assert_eq!(e.exit(), Exit::Shutdown { faded: true });
    let saved = std::fs::read(e.dir.path().join("state/current.json")).unwrap();
    let saved = iem_engine::persist::decode(&saved).unwrap();
    assert_eq!(saved.rev, 1);
    assert!(saved.saved_unix_ms > 1_700_000_000_000);
    assert_eq!(saved.topology_hash, h_hash(&e));
    // A second run loads that state.
    let again = Engine::start_in(e.dir, Flags::default(), InputSignal::Silence);
    let mut c = again.client();
    let (h, _, rev) = c.hello(Role::Observe);
    assert_eq!((h.state_rev, rev), (1, 1));
    let dir = again.shutdown();
    assert!(dir.path().join("state/gen-0000000001.json").exists());
}

#[test]
fn fault_injection_releases_the_driver_and_exits() {
    let off = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = off.client();
    c.hello(Role::Control);
    assert_eq!(
        c.request(1, Cmd::InjectFault).error.unwrap().code,
        ErrCode::Forbidden
    );
    off.shutdown();
    let mut e = Engine::start(
        Flags {
            test_signal: false,
            fault_injection: true,
        },
        InputSignal::Silence,
    );
    let mut c = e.client();
    c.hello(Role::Control);
    assert!(c.request(1, set_mix("member7", -5.0)).error.is_none());
    assert!(c.request(2, Cmd::InjectFault).error.is_none());
    let code = c.wait(|m| match m {
        EngineMsg::Alarm(a) if a.code == AlarmCode::Fault => Some(a.detail.clone()),
        _ => None,
    });
    assert!(code.contains("fault injection"), "{code}");
    c.wait(|m| {
        matches!(m, EngineMsg::DriverReleased { reason } if reason == "fault").then_some(())
    });
    assert!(matches!(e.exit(), Exit::Fault(why) if why.contains("fault injection")));
    let saved = std::fs::read(e.dir.path().join("state/current.json")).unwrap();
    assert_eq!(iem_engine::persist::decode(&saved).unwrap().rev, 1);
}

#[test]
fn the_binary_runs_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = pipe_name(&dir);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--site"])
        .arg(common::site_path())
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--pipe", &pipe])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client::new(&pipe);
    c.hello(Role::Control);
    assert!(c.request(1, Cmd::Shutdown).error.is_none());
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(start.elapsed() < WAIT, "the binary did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0));
    let usage = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .args(["run", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("iem-engine render"));
}

#[test]
fn the_binary_renders_offline() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.wav");
    let output = dir.path().join("out.wav");
    let mut audio = Planar::new(32, 960);
    for ch in 0..32 {
        audio.channel_mut(ch).fill(0.1);
    }
    wav::write_file(&input, 96_000, &audio).unwrap();
    let run = |extra: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .arg("render")
            .arg("--site")
            .arg(common::site_path())
            .arg("--in")
            .arg(&input)
            .arg("--out")
            .arg(&output)
            .args(extra)
            .output()
            .unwrap()
    };
    let ok = run(&["--block", "97"]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let (rate, out) = wav::read_file(&output).unwrap();
    assert_eq!((rate, out.channels(), out.frames()), (96_000, 21, 960));
    wav::write_file(&input, 48_000, &audio).unwrap();
    let refused = run(&[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("96000"));
}

#[test]
fn a_held_engine_sounds_after_its_supervisor_arms_it() {
    let e = Engine::start_with(tempfile::tempdir().unwrap(), |cfg| cfg.hold = true);
    let mut sup = e.client();
    let (h, _, _) = sup.hello(Role::Supervisor);
    assert_eq!((h.role, h.block), (Role::Supervisor, 32));
    let status = |held: bool| {
        move |m: &EngineMsg| match m {
            EngineMsg::Status(s) if s.held == held => Some(s.clone()),
            _ => None,
        }
    };
    let before = sup.wait(status(true));
    assert_eq!(
        (before.frames, before.missed, before.parked),
        (32, 0, false)
    );
    // Only the supervisor arms; the controller is told so.
    let mut ctl = e.client();
    ctl.hello(Role::Control);
    let refused = ctl.request(1, Cmd::Arm).error.unwrap();
    assert_eq!(refused.code, ErrCode::NotSupervisor);
    assert_eq!(
        ctl.request(2, set_mix("member1", -2.0)).error,
        None,
        "the controller still mixes"
    );
    assert_eq!(
        sup.request(3, set_mix("member1", -4.0))
            .error
            .map(|b| b.code),
        Some(ErrCode::NotController)
    );
    assert!(sup.request(4, Cmd::Arm).error.is_none());
    let after = sup.wait(status(false));
    assert_eq!(after.frames, 32);
    e.shutdown();
}

#[test]
fn the_binary_checks_a_site() {
    let check = |site: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(["check-site", "--site"])
            .arg(site)
            .output()
            .unwrap()
    };
    let ok = check(common::site_path().as_path());
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(summary["topology"], common::topology().hash.as_str());
    assert_eq!(summary["inputs"], 24);
    assert_eq!(summary["groups"], 1);
    assert_eq!(summary["mixes"], 11);
    assert_eq!(summary["card"], true);
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("site.toml");
    let text = std::fs::read_to_string(common::site_path()).unwrap();
    std::fs::write(&bad, text.replace("frames = 32", "frames = 64")).unwrap();
    let refused = check(bad.as_path());
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("frames must be 32"));
}

/// Off Windows the card cannot open: `run --backend asio` and `interlock`
/// are usage errors (exit 2), never a card refusal (exit 3).
#[cfg(not(windows))]
#[test]
fn off_windows_the_binary_refuses_the_card_as_usage() {
    let dir = tempfile::tempdir().unwrap();
    let engine = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_iem-engine"))
            .args(args)
            .arg("--site")
            .arg(common::site_path())
            .output()
            .unwrap()
    };
    let interlock = engine(&["interlock", "--seconds", "5"]);
    assert_eq!(interlock.status.code(), Some(2));
    assert!(interlock.stdout.is_empty());
    let state = dir.path().join("state").to_string_lossy().into_owned();
    let pipe = pipe_name(&dir);
    let asio = engine(&[
        "run",
        "--backend",
        "asio",
        "--state-dir",
        state.as_str(),
        "--pipe",
        pipe.as_str(),
    ]);
    assert_eq!(asio.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&asio.stderr).contains("Windows"));
}

/// Windows named pipes only (S6 design note §4): the private DACL, the
/// first-instance flag and writes bounded like the Unix send timeout.
#[cfg(windows)]
mod named_pipes {
    use super::*;
    use iem_engine::pipe::{listen, sddl_is_private};
    use iem_win::token::{current_user_sid, pipe_sddl, sddl_sid};

    #[test]
    fn a_client_that_stops_reading_is_dropped_and_the_engine_keeps_serving() {
        let e = Engine::start(
            Flags::default(),
            InputSignal::Sine {
                hz: 1000.0,
                amp: 0.1,
            },
        );
        let mut ctl = e.client();
        ctl.hello(Role::Control);
        let engineer = Cmd::StartListen {
            mix: MixId::new("engineer"),
        };
        assert!(ctl.request(1, engineer).error.is_none());
        // Two clients that never read: the engine's writes to them (the
        // topology and meters; listen frames) outgrow their pipes' 512-byte
        // buffers and wait for them.
        let pipe = e.pipe.clone();
        let stalled = connect(move || control_name(&pipe));
        let hello = ClientMsg::Hello {
            proto: PROTO,
            role: Role::Observe,
            client: "stalled".into(),
        };
        write_frame(&mut &stalled, &hello).unwrap();
        let pipe = e.pipe.clone();
        let stalled_media = connect(move || media_name(&pipe));
        let start = Instant::now();
        // The control thread gives the stalled client SEND_TIMEOUT (1 s) and
        // then drops it: the controller's replies keep coming.
        for id in 2..=6 {
            let asked = Instant::now();
            assert!(ctl.request(id, Cmd::Ping).error.is_none());
            let took = asked.elapsed();
            assert!(took < Duration::from_secs(2), "reply {id} after {took:?}");
        }
        // The media pump dropped its stalled client too: a new one is served.
        let media = media_client(&e.pipe);
        let (h, samples) = media.try_recv().unwrap();
        assert_eq!((h.channels, samples.len()), (2, 2 * FRAME_48K));
        // The stalled control client is gone: reading it now shows at most
        // what fitted its pipe before the drop, then the end.
        std::thread::sleep(Duration::from_secs(2).saturating_sub(start.elapsed()));
        let mut dropped = Client {
            r: Reader::start(stalled, Framer::next_frame),
        };
        assert!(dropped.closed(), "the stalled client was dropped");
        drop(stalled_media);
        drop(media);
        // `run` joins the media pump: the shutdown still ends it within 5 s.
        e.shutdown();
    }

    /// `ERROR_PIPE_BUSY`: every instance is taken for a moment.
    const BUSY: i32 = 231;

    /// The pipe's DACL; waits while the engine has not yet made a new
    /// instance after the last connection.
    fn read_dacl(name: &str) -> String {
        let start = Instant::now();
        loop {
            match pipe_sddl(name) {
                Ok(text) => return text,
                Err(e) if e.raw_os_error() == Some(BUSY) && start.elapsed() < WAIT => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{name}: {e}"),
            }
        }
    }

    #[test]
    fn a_second_listener_on_a_held_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let name = pipe_name(&dir);
        let _first = listen(control_name(&name).unwrap()).unwrap();
        let second = listen(control_name(&name).unwrap()).unwrap_err();
        assert_eq!(second.kind(), std::io::ErrorKind::AddrInUse, "{second}");
        assert!(
            second.to_string().starts_with("pipe name taken ("),
            "{second}"
        );
    }

    fn accept(listener: &interprocess::local_socket::Listener) -> Stream {
        use interprocess::local_socket::traits::Listener as _;
        let start = Instant::now();
        loop {
            match listener.accept() {
                Ok(s) => return s,
                Err(e) => {
                    assert!(start.elapsed() < WAIT, "accept: {e}");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    #[test]
    fn a_gone_listeners_name_is_free_while_its_client_still_holds_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let name = pipe_name(&dir);
        let first = listen(control_name(&name).unwrap()).unwrap();
        let n = name.clone();
        let client = connect(move || control_name(&n));
        drop(accept(&first));
        drop(first);
        // Every server end of the name is closed, as when an engine's
        // process has ended; its client still holds its end. A client's end
        // does not hold the name: a new listener (the respawned engine)
        // creates the first instance while the client is still there
        // (Windows CI run 36371298924 refuted the opposite).
        let again = listen(control_name(&name).unwrap())
            .unwrap_or_else(|e| panic!("the gone listener's client held the name: {e}"));
        // That client is no client of the new listener: its end reads the
        // end of the stream.
        let mut old = Client {
            r: Reader::start(client, Framer::next_frame),
        };
        assert!(old.closed(), "the gone listener's client reads the end");
        // A new client is the new listener's, which holds the name against
        // another listener.
        let n = name.clone();
        let fresh = connect(move || control_name(&n));
        drop(accept(&again));
        let squatter = listen(control_name(&name).unwrap()).unwrap_err();
        assert_eq!(squatter.kind(), std::io::ErrorKind::AddrInUse, "{squatter}");
        assert!(
            squatter.to_string().starts_with("pipe name taken ("),
            "{squatter}"
        );
        drop(fresh);
    }

    #[test]
    fn a_second_engine_on_a_held_pipe_stops_with_an_io_error() {
        let e = Engine::start(Flags::default(), InputSignal::Silence);
        e.client().hello(Role::Observe);
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunConfig::new(
            common::site_path(),
            dir.path().join("state"),
            e.pipe.clone(),
        );
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(cfg));
        });
        let err = rx
            .recv_timeout(WAIT)
            .expect("a refused engine returns at once")
            .unwrap_err();
        assert!(
            matches!(&err, EngineError::Io(io) if io.kind() == std::io::ErrorKind::AddrInUse),
            "{err}"
        );
        assert!(err.to_string().starts_with("pipe name taken ("), "{err}");
        e.shutdown();
    }

    #[test]
    fn the_engines_pipes_admit_only_the_user_and_system() {
        let e = Engine::start(Flags::default(), InputSignal::Silence);
        e.client().hello(Role::Observe);
        let user = current_user_sid().unwrap();
        let written = sddl_sid(&user).unwrap();
        // No media client is connected: reading the media pipe's DACL
        // connects as one for a moment (`pipe_sddl`), superseding nobody.
        for name in [e.pipe.clone(), format!("{}.media", e.pipe)] {
            let dacl = read_dacl(&name);
            assert!(
                sddl_is_private(&dacl, &written),
                "{name}: {dacl} (user {user}, written {written})"
            );
            assert!(dacl.contains(";;;SY)"), "{name}: {dacl}");
            // Protected: nothing is inherited into it.
            assert!(dacl.starts_with("D:P("), "{name}: {dacl}");
        }
        e.shutdown();
    }
}
