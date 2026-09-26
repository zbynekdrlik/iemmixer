//! The engine client (S5 design note §4): the server is the engine's one
//! controller. One task keeps the control pipe connected (hello as
//! controller, then `Hello`, `Topology`, `State`), feeds every engine message
//! into the [`Mirror`], answers requests by id, asks for the state again on a
//! revision gap, and reconnects after a drop or a `Superseded`
//! (0.25 → 2 s backoff; 5 s after being superseded).

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::Duration;

use iem_engine_proto::{Alarm, ClientMsg, Cmd, EngineMsg, ErrorBody, Meters, PROTO, Role, Status};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::mirror::{Mirror, MirrorEvent};
use super::wire::{self, WireError};

/// How long a request waits for its reply.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// How long `request_applied` waits for the mirror to reach the reply's revision.
pub const APPLY_TIMEOUT: Duration = Duration::from_secs(1);
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(2);
pub const SUPERSEDED_WAIT: Duration = Duration::from_secs(5);

pub type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub type Writer = Box<dyn AsyncWrite + Unpin + Send>;
pub type ConnectFuture = Pin<Box<dyn Future<Output = io::Result<(Reader, Writer)>> + Send>>;
/// Opens one connection to the engine's control pipe.
pub type Connector = Arc<dyn Fn() -> ConnectFuture + Send + Sync>;

/// What happened at the engine, for the server's views.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Hello, topology and state arrived: the mirror is live.
    Connected,
    /// The pipe dropped (or the engine superseded us).
    Disconnected,
    /// The whole state was replaced (resync, import).
    Reset,
    /// Changes at the next revision; `origin` is the session that caused them.
    Changed {
        origin: Option<u64>,
        changes: Arc<Vec<iem_engine_proto::Change>>,
    },
    Meters(Arc<Meters>),
    Status(Status),
    Alarm(Alarm),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("the engine is not connected")]
    Disconnected,
    #[error("the engine refused: {0:?}")]
    Refused(ErrorBody),
    #[error("the engine did not answer in time")]
    Timeout,
}

struct Inner {
    connector: Connector,
    client: String,
    mirror: RwLock<Mirror>,
    events: broadcast::Sender<EngineEvent>,
    out: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<u64, ErrorBody>>>>,
    next_id: AtomicU64,
    connected: AtomicBool,
    rev: watch::Sender<u64>,
}

/// A handle on the engine connection (cheap to clone).
#[derive(Clone)]
pub struct EngineClient {
    inner: Arc<Inner>,
}

/// Why one connection ended.
#[derive(Debug, PartialEq)]
enum End {
    Dropped,
    Superseded,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The next backoff after a failed attempt.
pub fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(BACKOFF_MAX)
}

/// The real connector: the control pipe `pipe`.
pub fn pipe_connector(pipe: String) -> Connector {
    Arc::new(move || -> ConnectFuture {
        let pipe = pipe.clone();
        Box::pin(async move { wire::connect(&pipe).await })
    })
}

impl EngineClient {
    /// Connects to the engine's control pipe and keeps it connected.
    pub fn spawn(pipe: String, build: String) -> Self {
        Self::spawn_with(pipe_connector(pipe), build)
    }

    /// Like [`EngineClient::spawn`] with another way to reach the engine (tests).
    pub fn spawn_with(connector: Connector, build: String) -> Self {
        let client = Self::build(connector, build);
        let task = client.clone();
        tokio::spawn(async move { task.run().await });
        client
    }

    /// A client that never connects (code that must work without an engine,
    /// and the state before start-up connects it).
    pub fn detached() -> Self {
        let connector: Connector =
            Arc::new(|| -> ConnectFuture { Box::pin(async { Err(io::Error::other("detached")) }) });
        Self::build(connector, "detached".into())
    }

    fn build(connector: Connector, build: String) -> Self {
        let (events, _) = broadcast::channel(1024);
        let (rev, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                connector,
                client: format!("iem-server {build}"),
                mirror: RwLock::new(Mirror::default()),
                events,
                out: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                connected: AtomicBool::new(false),
                rev,
            }),
        }
    }

    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    /// The mirror (never hold it across an `.await`).
    pub fn mirror(&self) -> RwLockReadGuard<'_, Mirror> {
        self.inner
            .mirror
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    fn emit(&self, e: EngineEvent) {
        let _ = self.inner.events.send(e);
    }

    /// Sends `cmd` and waits for its reply: the revision after it.
    pub async fn request(&self, cmd: Cmd, origin: Option<u64>) -> Result<u64, EngineError> {
        if !self.connected() {
            return Err(EngineError::Disconnected);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.inner.pending).insert(id, tx);
        let frame = encode(&ClientMsg::Request { id, origin, cmd });
        let sent = match (frame, lock(&self.inner.out).as_ref()) {
            (Some(bytes), Some(out)) => out.send(bytes).is_ok(),
            _ => false,
        };
        if !sent {
            lock(&self.inner.pending).remove(&id);
            return Err(EngineError::Disconnected);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(rev))) => Ok(rev),
            Ok(Ok(Err(body))) => Err(EngineError::Refused(body)),
            Ok(Err(_)) => Err(EngineError::Disconnected),
            Err(_) => {
                lock(&self.inner.pending).remove(&id);
                Err(EngineError::Timeout)
            }
        }
    }

    /// Sends `cmd`, then waits until the mirror holds its effect.
    pub async fn request_applied(&self, cmd: Cmd, origin: Option<u64>) -> Result<(), EngineError> {
        let rev = self.request(cmd, origin).await?;
        let mut watch = self.inner.rev.subscribe();
        let reached = tokio::time::timeout(APPLY_TIMEOUT, watch.wait_for(|r| *r >= rev)).await;
        match reached {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(EngineError::Disconnected),
            Err(_) => Err(EngineError::Timeout),
        }
    }

    async fn run(&self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            let end = match (self.inner.connector)().await {
                Ok((r, w)) => {
                    let end = self.connection(r, w).await;
                    if self.connected() {
                        backoff = BACKOFF_MIN;
                    }
                    Some(end)
                }
                Err(e) => {
                    tracing::debug!(error = %e, "engine pipe not reachable");
                    None
                }
            };
            self.teardown();
            let wait = match end {
                Some(End::Superseded) => SUPERSEDED_WAIT,
                _ => backoff,
            };
            backoff = next_backoff(backoff);
            tokio::time::sleep(wait).await;
        }
    }

    /// Everything a dropped connection leaves behind.
    fn teardown(&self) {
        *lock(&self.inner.out) = None;
        for (_, tx) in lock(&self.inner.pending).drain() {
            drop(tx);
        }
        self.inner
            .mirror
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .disconnected();
        if self.inner.connected.swap(false, Ordering::AcqRel) {
            tracing::warn!("engine disconnected");
            self.emit(EngineEvent::Disconnected);
        }
    }

    async fn connection(&self, mut r: Reader, mut w: Writer) -> End {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let hello = ClientMsg::Hello {
            proto: PROTO,
            role: Role::Control,
            client: self.inner.client.clone(),
        };
        if let Some(bytes) = encode(&hello) {
            let _ = tx.send(bytes);
        }
        *lock(&self.inner.out) = Some(tx);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(bytes) = rx.recv().await {
                if w.write_all(&bytes).await.is_err() || w.flush().await.is_err() {
                    break;
                }
            }
        });
        let end = loop {
            let body = match wire::read_frame(&mut r).await {
                Ok(b) => b,
                Err(WireError::Closed) => break End::Dropped,
                Err(e) => {
                    tracing::warn!(error = %e, "engine pipe read failed");
                    break End::Dropped;
                }
            };
            let msg: EngineMsg = match serde_json::from_slice(&body) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "unreadable engine message skipped");
                    continue;
                }
            };
            if let Some(end) = self.handle(msg) {
                break end;
            }
        };
        writer.abort();
        end
    }

    /// One engine message; `Some` ends the connection.
    fn handle(&self, msg: EngineMsg) -> Option<End> {
        match msg {
            EngineMsg::Reply(reply) => {
                if let Some(tx) = lock(&self.inner.pending).remove(&reply.id) {
                    let _ = tx.send(match reply.error {
                        None => Ok(reply.rev),
                        Some(e) => Err(e),
                    });
                } else if let Some(e) = reply.error {
                    tracing::warn!(code = ?e.code, msg = %e.msg, "engine error without a request");
                }
                None
            }
            EngineMsg::Meters(m) => {
                self.emit(EngineEvent::Meters(Arc::new(m)));
                None
            }
            EngineMsg::Status(s) => {
                self.emit(EngineEvent::Status(s));
                None
            }
            EngineMsg::Alarm(a) => {
                tracing::warn!(code = ?a.code, detail = %a.detail, "engine alarm");
                self.emit(EngineEvent::Alarm(a));
                None
            }
            EngineMsg::Superseded => {
                tracing::error!("another controller took over the engine");
                Some(End::Superseded)
            }
            EngineMsg::DriverReleased { reason } => {
                tracing::warn!(%reason, "the engine released its driver");
                None
            }
            EngineMsg::Saved { .. } => None,
            other => {
                let event = {
                    let mut m = self
                        .inner
                        .mirror
                        .write()
                        .unwrap_or_else(PoisonError::into_inner);
                    let event = m.apply(&other);
                    self.inner.rev.send_replace(m.rev);
                    event
                };
                match event {
                    Some(MirrorEvent::Reset) => {
                        if !self.inner.connected.swap(true, Ordering::AcqRel) {
                            tracing::info!("engine connected");
                            self.emit(EngineEvent::Connected);
                        }
                        self.emit(EngineEvent::Reset);
                    }
                    Some(MirrorEvent::Changed { origin, changes }) => {
                        self.emit(EngineEvent::Changed {
                            origin,
                            changes: Arc::new(changes),
                        });
                    }
                    Some(MirrorEvent::Resync) => {
                        tracing::warn!("engine revision gap: fetching the state");
                        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
                        let get = encode(&ClientMsg::Request {
                            id,
                            origin: None,
                            cmd: Cmd::GetState,
                        });
                        if let (Some(bytes), Some(out)) = (get, lock(&self.inner.out).as_ref()) {
                            let _ = out.send(bytes);
                        }
                    }
                    Some(MirrorEvent::Topology) | None => {}
                }
                None
            }
        }
    }
}

fn encode(msg: &ClientMsg) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match iem_engine_proto::write_frame(&mut out, msg) {
        Ok(()) => Some(out),
        Err(e) => {
            tracing::error!(error = %e, "cannot encode a request");
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted engine on an in-memory pipe, for tests of the client and
    //! of the code above it that must not depend on a real engine.

    use super::*;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    /// The engine's end of one connection.
    pub struct Peer {
        pub r: ReadHalf<DuplexStream>,
        pub w: WriteHalf<DuplexStream>,
    }

    impl Peer {
        pub async fn recv(&mut self) -> ClientMsg {
            let body = tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut self.r))
                .await
                .expect("a client message within 5 s")
                .expect("a frame");
            serde_json::from_slice(&body).expect("a client message")
        }

        pub async fn send(&mut self, msg: &EngineMsg) {
            wire::write_msg(&mut self.w, msg).await.expect("send");
        }
    }

    /// A connector handing out in-memory pipes; the engine side of every
    /// connection arrives on the returned receiver.
    pub fn connector() -> (Connector, mpsc::UnboundedReceiver<Peer>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let connector: Connector = Arc::new(move || -> ConnectFuture {
            let tx = tx.clone();
            Box::pin(async move {
                let (client, engine) = tokio::io::duplex(1 << 20);
                let (cr, cw) = tokio::io::split(client);
                let (er, ew) = tokio::io::split(engine);
                tx.send(Peer { r: er, w: ew })
                    .map_err(|_| io::Error::other("test over"))?;
                Ok((Box::new(cr) as Reader, Box::new(cw) as Writer))
            })
        });
        (connector, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Peer, connector};
    use super::*;
    use iem_engine_proto::{
        Change, ErrCode, Hello, InputId, Level, MixId, MixState, Reply, Source, TopologyInfo,
        Transient,
    };

    fn hello() -> EngineMsg {
        EngineMsg::Hello(Hello {
            proto: 1,
            engine_build: "e".into(),
            topology_hash: "h".into(),
            state_rev: 3,
            sample_rate: 96_000,
            block: 32,
            role: Role::Control,
        })
    }

    fn topo() -> EngineMsg {
        EngineMsg::Topology(TopologyInfo {
            hash: "h".into(),
            sample_rate: 96_000,
            engineer: MixId::new("engineer"),
            inputs: vec![],
            groups: vec![],
            mixes: vec![],
        })
    }

    fn state(rev: u64) -> EngineMsg {
        EngineMsg::State {
            rev,
            state: MixState::default(),
            transient: Transient::default(),
        }
    }

    async fn next_peer(rx: &mut mpsc::UnboundedReceiver<Peer>) -> Peer {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a connection within 5 s")
            .expect("a peer")
    }

    async fn event(rx: &mut broadcast::Receiver<EngineEvent>) -> EngineEvent {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an event within 5 s")
            .expect("an event")
    }

    /// A client connected to a fake engine that said hello, topology, state.
    async fn connected() -> (
        EngineClient,
        Peer,
        broadcast::Receiver<EngineEvent>,
        mpsc::UnboundedReceiver<Peer>,
    ) {
        let (c, mut peers) = connector();
        let client = EngineClient::spawn_with(c, "t".into());
        let mut events = client.subscribe();
        let mut peer = next_peer(&mut peers).await;
        match peer.recv().await {
            ClientMsg::Hello {
                proto,
                role,
                client,
            } => {
                assert_eq!((proto, role), (PROTO, Role::Control));
                assert_eq!(client, "iem-server t");
            }
            other => panic!("expected hello, got {other:?}"),
        }
        assert!(!client.connected());
        peer.send(&hello()).await;
        peer.send(&topo()).await;
        peer.send(&state(3)).await;
        assert!(matches!(event(&mut events).await, EngineEvent::Connected));
        assert!(matches!(event(&mut events).await, EngineEvent::Reset));
        assert!(client.connected());
        assert_eq!(client.mirror().rev, 3);
        (client, peer, events, peers)
    }

    #[tokio::test]
    async fn requests_are_answered_by_id() {
        let (client, mut peer, _events, _peers) = connected().await;
        let c2 = client.clone();
        let call = tokio::spawn(async move { c2.request(Cmd::Ping, Some(9)).await });
        let ClientMsg::Request { id, origin, cmd } = peer.recv().await else {
            panic!("expected a request");
        };
        assert_eq!((origin, cmd), (Some(9), Cmd::Ping));
        // A reply for another id is not ours.
        peer.send(&EngineMsg::Reply(Reply {
            id: id + 100,
            rev: 1,
            error: None,
        }))
        .await;
        peer.send(&EngineMsg::Reply(Reply {
            id,
            rev: 3,
            error: None,
        }))
        .await;
        assert_eq!(call.await.unwrap(), Ok(3));
        let c3 = client.clone();
        let refused = tokio::spawn(async move { c3.request(Cmd::SaveNow, None).await });
        let ClientMsg::Request { id, .. } = peer.recv().await else {
            panic!("expected a request");
        };
        let body = ErrorBody {
            code: ErrCode::UnknownId,
            msg: "no".into(),
        };
        peer.send(&EngineMsg::Reply(Reply {
            id,
            rev: 3,
            error: Some(body.clone()),
        }))
        .await;
        assert_eq!(refused.await.unwrap(), Err(EngineError::Refused(body)));
    }

    #[tokio::test]
    async fn an_applied_request_is_in_the_mirror() {
        let (client, mut peer, mut events, _peers) = connected().await;
        let c2 = client.clone();
        let mic = Source::Input(InputId::new("mic1"));
        let cmd = Cmd::SetLevel {
            mix: MixId::new("member1"),
            source: mic.clone(),
            gain_db: Some(-6.0),
            pan: None,
            muted: None,
        };
        let call = tokio::spawn(async move { c2.request_applied(cmd, Some(4)).await });
        let ClientMsg::Request { id, .. } = peer.recv().await else {
            panic!("expected a request");
        };
        peer.send(&EngineMsg::Reply(Reply {
            id,
            rev: 4,
            error: None,
        }))
        .await;
        let level = Level {
            gain_db: -6.0,
            ..Level::default()
        };
        peer.send(&EngineMsg::Delta {
            rev: 4,
            origin: Some(4),
            changes: vec![Change::Level {
                mix: MixId::new("member1"),
                source: mic.clone(),
                level,
            }],
        })
        .await;
        assert_eq!(call.await.unwrap(), Ok(()));
        assert_eq!(client.mirror().level(&MixId::new("member1"), &mic), level);
        match event(&mut events).await {
            EngineEvent::Changed { origin, changes } => {
                assert_eq!(origin, Some(4));
                assert_eq!(changes.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_request_without_an_answer_times_out() {
        let (client, mut peer, _events, _peers) = connected().await;
        let call = tokio::spawn(async move { client.request(Cmd::Ping, None).await });
        let _ = peer.recv().await;
        let got = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("the request ends within 5 s")
            .unwrap();
        assert_eq!(got, Err(EngineError::Timeout));
    }

    #[tokio::test]
    async fn a_rev_gap_resyncs() {
        let (client, mut peer, mut events, _peers) = connected().await;
        peer.send(&EngineMsg::Delta {
            rev: 5,
            origin: None,
            changes: vec![],
        })
        .await;
        let ClientMsg::Request { cmd, .. } = peer.recv().await else {
            panic!("expected a request");
        };
        assert_eq!(cmd, Cmd::GetState);
        peer.send(&state(5)).await;
        assert!(matches!(event(&mut events).await, EngineEvent::Reset));
        assert_eq!(client.mirror().rev, 5);
        assert!(client.connected());
    }

    #[tokio::test]
    async fn a_dropped_pipe_disconnects_fails_requests_and_reconnects() {
        let (client, peer, mut events, mut peers) = connected().await;
        drop(peer);
        assert!(matches!(
            event(&mut events).await,
            EngineEvent::Disconnected
        ));
        assert!(!client.connected());
        assert_eq!(
            client.request(Cmd::Ping, None).await,
            Err(EngineError::Disconnected)
        );
        let mut again = next_peer(&mut peers).await;
        assert!(matches!(again.recv().await, ClientMsg::Hello { .. }));
        again.send(&hello()).await;
        again.send(&topo()).await;
        again.send(&state(8)).await;
        assert!(matches!(event(&mut events).await, EngineEvent::Connected));
        assert_eq!(client.mirror().rev, 8);
    }

    #[tokio::test]
    async fn meters_status_and_alarms_are_forwarded_and_superseded_disconnects() {
        let (client, mut peer, mut events, _peers) = connected().await;
        peer.send(&EngineMsg::Meters(Meters {
            seq: 7,
            ..Meters::default()
        }))
        .await;
        assert!(matches!(event(&mut events).await, EngineEvent::Meters(m) if m.seq == 7));
        peer.send(&EngineMsg::Status(Status {
            callbacks: 3,
            ..Status::default()
        }))
        .await;
        assert!(matches!(event(&mut events).await, EngineEvent::Status(s) if s.callbacks == 3));
        peer.send(&EngineMsg::Alarm(Alarm {
            code: iem_engine_proto::AlarmCode::StateLost,
            detail: "d".into(),
        }))
        .await;
        assert!(matches!(event(&mut events).await, EngineEvent::Alarm(_)));
        peer.send(&EngineMsg::Saved {
            rev: 3,
            generation: 1,
        })
        .await;
        peer.send(&EngineMsg::DriverReleased { reason: "x".into() })
            .await;
        peer.send(&EngineMsg::Superseded).await;
        assert!(matches!(
            event(&mut events).await,
            EngineEvent::Disconnected
        ));
        assert!(!client.connected());
    }

    #[tokio::test]
    async fn a_detached_client_never_connects() {
        let c = EngineClient::detached();
        assert!(!c.connected());
        assert_eq!(
            c.request(Cmd::Ping, None).await,
            Err(EngineError::Disconnected)
        );
        assert!(c.mirror().topology.is_none());
        assert!((c.inner.connector)().await.is_err());
    }

    #[test]
    fn backoff_doubles_up_to_two_seconds() {
        assert_eq!(next_backoff(BACKOFF_MIN), Duration::from_millis(500));
        assert_eq!(next_backoff(Duration::from_millis(1500)), BACKOFF_MAX);
        assert_eq!(next_backoff(BACKOFF_MAX), BACKOFF_MAX);
    }
}
