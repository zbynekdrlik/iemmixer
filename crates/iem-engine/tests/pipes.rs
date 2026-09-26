//! The engine through its pipes (design note §3.6, §3.7): an in-process
//! engine on NullRt at B = 32 with a temporary state directory, and the
//! `iem-engine` binary. Every read has a 5 s timeout; every wait is bounded.

mod common;

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use iem_audio_io::{InputSignal, Planar, wav};
use iem_engine::control::Exit;
use iem_engine::core::Flags;
use iem_engine::engine::{EngineError, RunConfig, run};
use iem_engine::pipe::{control_name, media_name};
use iem_engine_proto::media::stream;
use iem_engine_proto::{
    AlarmCode, BusId, Change, ClientMsg, Cmd, EngineMsg, ErrCode, FRAME_48K, FrameError, Hello,
    InputId, MAX_FRAME, MediaHeader, PROTO, Reply, Role, Source, TopologyInfo, read_frame,
    read_media, write_frame, write_media,
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
            Ok(s) => {
                s.set_recv_timeout(Some(WAIT)).unwrap();
                return s;
            }
            Err(e) if start.elapsed() < WAIT => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("cannot connect: {e}"),
        }
    }
}

struct Client {
    s: Stream,
}

impl Client {
    fn new(pipe: &str) -> Self {
        let p = pipe.to_owned();
        Self {
            s: connect(move || control_name(&p)),
        }
    }

    fn send(&mut self, msg: &ClientMsg) {
        write_frame(&mut self.s, msg).unwrap();
    }

    fn try_recv(&mut self) -> Result<EngineMsg, FrameError> {
        let mut buf = Vec::new();
        read_frame(&mut self.s, &mut buf)?;
        Ok(serde_json::from_slice(&buf).unwrap())
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
                Err(FrameError::Io(e)) => return e.kind() != std::io::ErrorKind::WouldBlock,
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
        let pipe = pipe_name(&dir);
        let mut cfg = RunConfig::new(common::site_path(), dir.path().join("state"), pipe.clone());
        cfg.flags = flags;
        cfg.signal = signal;
        cfg.solo_grace = Duration::from_millis(400);
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
        assert_eq!(self.exit(), Exit::Shutdown);
        self.dir
    }
}

fn set_bus(bus: &str, fader_db: f64) -> Cmd {
    Cmd::SetBus {
        bus: BusId::new(bus),
        fader_db: Some(fader_db),
        pan: None,
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
        (topo.inputs.len(), topo.buses.len(), topo.sends.len()),
        (24, 22, 268)
    );
    assert!(h.engine_build.starts_with(env!("CARGO_PKG_VERSION")));
    let mut obs = e.client();
    obs.hello(Role::Observe);
    let reply = ctl.request(7, set_bus("member1", -3.0));
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
            matches!(&changes[..], [Change::Bus { id, state }] if id.0 == "member1" && state.fader_db == -3.0)
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
    assert_eq!(state.1.buses[&BusId::new("member1")].fader_db, -3.0);
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
    let r = obs.request(2, set_bus("member2", -1.0));
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
    assert!(second.request(1, set_bus("member3", -2.0)).error.is_none());
    e.shutdown();
}

#[test]
fn garbage_gets_a_typed_error_and_oversize_closes_only_that_connection() {
    let e = Engine::start(Flags::default(), InputSignal::Silence);
    let mut c = e.client();
    c.hello(Role::Control);
    let mut raw = 3u32.to_le_bytes().to_vec();
    raw.extend_from_slice(b"xyz");
    c.s.write_all(&raw).unwrap();
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
    bad.s
        .write_all(&((MAX_FRAME + 1) as u32).to_le_bytes())
        .unwrap();
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
    c.hello(Role::Observe);
    let start = Instant::now();
    let mut meters = 0;
    let mut loud = false;
    let mut status = None;
    while start.elapsed() < Duration::from_secs(3) && (meters < 3 || status.is_none()) {
        match c.recv() {
            EngineMsg::Meters(m) => {
                meters += 1;
                assert_eq!((m.inputs.len(), m.buses.len(), m.gr_db.len()), (24, 22, 22));
                loud |= m.inputs.iter().all(|p| p[0] > 0.05);
            }
            EngineMsg::Status(s) => status = Some(s),
            _ => {}
        }
    }
    assert!(meters >= 3, "{meters} meter frames");
    assert!(loud, "the sine shows on every input meter");
    let s = status.expect("a status within a second");
    assert!(s.callbacks > 0 && !s.faulted);
    e.shutdown();
}

fn media_client(pipe: &str) -> Stream {
    let p = pipe.to_owned();
    connect(move || media_name(&p))
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
                bus: BusId::new("engineer")
            }
        )
        .error
        .is_none()
    );
    assert!(
        c.request(
            2,
            Cmd::StartListen {
                bus: BusId::new("member4")
            }
        )
        .error
        .is_none()
    );
    let r = c.request(
        3,
        Cmd::StartListen {
            bus: BusId::new("member5"),
        },
    );
    assert_eq!(r.error.unwrap().code, ErrCode::NoSource);
    let mut m = media_client(&e.pipe);
    let mut seen = [0u64; 2];
    let mut samples = Vec::new();
    let start = Instant::now();
    while (seen[0] < 3 || seen[1] < 3) && start.elapsed() < WAIT {
        let h = read_media(&mut m, &mut samples).unwrap();
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
    let mut m = media_client(&e.pipe);
    let frame = vec![0.5f32; FRAME_48K];
    let start = Instant::now();
    let mut heard = false;
    let mut seq = 0;
    while !heard && start.elapsed() < WAIT {
        write_media(
            &mut m,
            &MediaHeader {
                stream: stream::TALKBACK,
                channels: 1,
                seq,
                frames: FRAME_48K as u16,
            },
            &frame,
        )
        .unwrap();
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
        scope: BusId::new("member3"),
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
    assert!(c.request(1, set_bus("member6", -4.0)).error.is_none());
    let mut e = e;
    let r = c.request(2, Cmd::Shutdown);
    assert!(r.error.is_none());
    c.wait(|m| {
        matches!(m, EngineMsg::DriverReleased { reason } if reason == "shutdown").then_some(())
    });
    assert_eq!(e.exit(), Exit::Shutdown);
    assert!(e.dir.path().join("state/current.json").exists());
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
    assert!(c.request(1, set_bus("member7", -5.0)).error.is_none());
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
    assert_eq!((rate, out.channels(), out.frames()), (96_000, 23, 960));
    wav::write_file(&input, 48_000, &audio).unwrap();
    let refused = run(&[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("96000"));
}
