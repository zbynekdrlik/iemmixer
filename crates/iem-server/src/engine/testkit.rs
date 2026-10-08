//! A real engine for the server's tests (S5 design note §7): `iem-engine`'s
//! `run` on a thread with the NullRt backend, the test site and a pipe in a
//! temporary directory; no mock of the engine. Unix only (the engine's pipe
//! readers need socket timeouts; S6 reworks Windows).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iem_engine::engine::{RunConfig, run};
use iem_engine_proto::{ClientMsg, Cmd, EngineMsg, ErrorBody, PROTO, Role};

use crate::AppState;
use crate::engine::EngineClient;

pub fn site_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
}

/// Polls `f` every 20 ms for up to 5 s.
pub async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub struct EngineHarness {
    pub dir: tempfile::TempDir,
    pub pipe: String,
    pub state_dir: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EngineHarness {
    pub fn start() -> Self {
        Self::start_with(|_| {})
    }

    pub fn start_with(f: impl FnOnce(&mut RunConfig)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let pipe = dir
            .path()
            .join("engine.sock")
            .to_string_lossy()
            .into_owned();
        let state_dir = dir.path().join("state");
        let thread = Self::spawn(&pipe, &state_dir, f);
        Self {
            dir,
            pipe,
            state_dir,
            thread: Some(thread),
        }
    }

    fn spawn(
        pipe: &str,
        state_dir: &Path,
        f: impl FnOnce(&mut RunConfig),
    ) -> std::thread::JoinHandle<()> {
        let mut cfg = RunConfig::new(site_path(), state_dir.to_path_buf(), pipe.to_string());
        f(&mut cfg);
        let thread = std::thread::spawn(move || {
            if let Err(e) = run(cfg) {
                eprintln!("test engine: {e}");
            }
        });
        let t0 = Instant::now();
        let media = format!("{pipe}.media");
        while !(Path::new(pipe).exists() && Path::new(&media).exists()) {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "the engine's pipes within 5 s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        thread
    }

    /// Waits (≤ 10 s) for an engine that was told to shut down, then starts
    /// a new one on the same pipe and state directory.
    pub fn start_again(&mut self) {
        if let Some(t) = self.thread.take() {
            assert!(
                join_within(t, Duration::from_secs(10)),
                "the engine stopped"
            );
        }
        self.thread = Some(Self::spawn(&self.pipe, &self.state_dir, |_| {}));
    }

    /// `Shutdown` through a controller connection of its own, read until the
    /// engine answered it (the engine drops a peer whose socket closed before
    /// its hello was answered, and would never see the request); then waits
    /// ≤ 10 s for the engine thread — never forever.
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if let Ok(mut s) = std::os::unix::net::UnixStream::connect(&self.pipe) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
            let hello = ClientMsg::Hello {
                proto: PROTO,
                role: Role::Control,
                client: "test shutdown".into(),
            };
            let bye = ClientMsg::Request {
                id: 1,
                origin: None,
                cmd: Cmd::Shutdown,
            };
            let _ = iem_engine_proto::write_frame(&mut s, &hello);
            let _ = iem_engine_proto::write_frame(&mut s, &bye);
            let t0 = Instant::now();
            let mut buf = Vec::new();
            while t0.elapsed() < Duration::from_secs(5)
                && iem_engine_proto::read_frame(&mut s, &mut buf).is_ok()
            {
                if matches!(
                    serde_json::from_slice::<EngineMsg>(&buf),
                    Ok(EngineMsg::Reply(r)) if r.id == 1
                ) {
                    break;
                }
            }
        }
        if !join_within(thread, Duration::from_secs(10)) {
            eprintln!("the test engine did not stop within 10 s; left running");
        }
    }

    /// `cmd` through a supervisor connection of its own (the guard's role,
    /// S6; e.g. a HIL signal under `--test-signal`), in the pattern of
    /// [`Self::shutdown`]: read until the engine answered it; the answer's
    /// error, if any. Bounded: each read ≤ 1 s, the exchange ≤ 5 s.
    pub fn supervise(&self, cmd: Cmd) -> Result<(), ErrorBody> {
        let mut s = std::os::unix::net::UnixStream::connect(&self.pipe).expect("the control pipe");
        s.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a read timeout");
        let hello = ClientMsg::Hello {
            proto: PROTO,
            role: Role::Supervisor,
            client: "test supervisor".into(),
        };
        let request = ClientMsg::Request {
            id: 1,
            origin: None,
            cmd,
        };
        iem_engine_proto::write_frame(&mut s, &hello).expect("the hello");
        iem_engine_proto::write_frame(&mut s, &request).expect("the request");
        let t0 = Instant::now();
        let mut buf = Vec::new();
        while t0.elapsed() < Duration::from_secs(5) {
            iem_engine_proto::read_frame(&mut s, &mut buf)
                .expect("a frame from the engine within 1 s");
            if let Ok(EngineMsg::Reply(r)) = serde_json::from_slice::<EngineMsg>(&buf)
                && r.id == 1
            {
                return match r.error {
                    None => Ok(()),
                    Some(e) => Err(e),
                };
            }
        }
        panic!("the engine did not answer the supervisor's request within 5 s");
    }

    /// An app state connected to this engine (control and media pipes).
    pub async fn state(&self) -> (tempfile::TempDir, AppState) {
        let config_dir = tempfile::tempdir().expect("temp dir");
        let mut config = crate::site_view::tests::test_config();
        config.engine_pipe = self.pipe.clone();
        config.jwt_secret = "h".repeat(24);
        let mut state = AppState::new(config, config_dir.path());
        state.engine = EngineClient::spawn(self.pipe.clone(), "test".into());
        #[cfg(feature = "audio")]
        {
            state.media = crate::engine::media::MediaLink::spawn(self.pipe.clone());
        }
        let engine = state.engine.clone();
        wait_until("the engine connection", || engine.connected()).await;
        (config_dir, state)
    }
}

/// Joins `thread` if it ends within `limit`; otherwise leaves it running.
fn join_within(thread: std::thread::JoinHandle<()>, limit: Duration) -> bool {
    let t0 = Instant::now();
    while !thread.is_finished() {
        if t0.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = thread.join();
    true
}

impl Drop for EngineHarness {
    fn drop(&mut self) {
        self.shutdown();
    }
}
