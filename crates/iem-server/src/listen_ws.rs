//! `/ws/audio` — Listen (F15, F17; X3, X4): the engineer listens to their
//! own mix (the engine's tap after the limiter, slot 0) or one other mix
//! (after its mute, through the listen limiter, slot 1) as binary Opus
//! frames; the wire format is the predecessor's. `ListenStart{member_id}`
//! starts the engine's tap; the last listener of a mix leaving stops it; a
//! second different mix while one is heard gets `no_source`.
//!
//! The listen probe (S7, #10): a session opened with `&hil=1` (engineer-only
//! like every `/ws/audio` socket) also takes its slot's probe channel and,
//! while probe frames come, sends them in place of the slot's own (silent)
//! frames (`probe_gate`), telling the socket `AudioStatus` `probe` at a
//! burst's first frame and `listening` after its last. Any other session
//! never holds a probe receiver (`feeds`), whatever mix it hears.

pub mod probe_gate;

use std::time::Duration;

use axum::{
    Json,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use bytes::Bytes;
use iem_core::{ApiError, ClientMsg, ServerMsg};
use iem_engine_proto::{Cmd, ErrCode, MixId};
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::AppState;
use crate::engine::client::EngineError;
use crate::engine::media::MediaLink;
use crate::mixer_ws::{WsQuery, claims_of};
use probe_gate::{Pass, ProbeGate};

type Reject = (StatusCode, Json<ApiError>);

/// A listener more than this many frames behind skips to the newest.
pub const MAX_BEHIND: usize = 4;

pub async fn ws_audio(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> Result<impl IntoResponse, Reject> {
    let claims = {
        let config = state.config.read().await;
        claims_of(query.token.as_deref(), &config.jwt_secret)?
    };
    if !claims.engineer {
        tracing::warn!(sub = %claims.sub, "Audio WS denied: not engineer");
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "FORBIDDEN",
                "Audio streaming is engineer-only",
            )),
        ));
    }
    let hil = asks_for_probe(&query);
    Ok(ws.on_upgrade(move |socket| session(socket, state, hil)))
}

/// `&hil=1` (S7): the session also takes its slot's listen probe. Any other
/// value, or none, is a plain listener.
pub fn asks_for_probe(query: &WsQuery) -> bool {
    query.hil == Some(1)
}

fn status(s: &str, target: Option<String>) -> Message {
    Message::Text(
        serde_json::to_string(&ServerMsg::AudioStatus {
            status: s.into(),
            target,
        })
        .unwrap_or_default()
        .into(),
    )
}

/// The listen slot of `mix` (0: the engineer's mix).
pub fn slot_of(state: &AppState, mix: &MixId) -> usize {
    let engineer = state
        .engine
        .mirror()
        .topology
        .as_ref()
        .map(|t| t.engineer.clone());
    usize::from(engineer.as_ref() != Some(mix))
}

/// Registers one more listener of `mix`; the first one starts the tap.
async fn start(state: &AppState, mix: &MixId) -> Result<(), EngineError> {
    state
        .engine
        .request(Cmd::StartListen { mix: mix.clone() }, None)
        .await?;
    *state
        .listeners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(mix.clone())
        .or_default() += 1;
    Ok(())
}

/// One listener of `mix` left; the last one stops the tap.
async fn stop(state: &AppState, mix: &MixId) {
    let last = {
        let mut l = state
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match l.get_mut(mix) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                l.remove(mix);
                true
            }
            None => false,
        }
    };
    if last
        && let Err(e) = state
            .engine
            .request(Cmd::StopListen { mix: mix.clone() }, None)
            .await
    {
        tracing::warn!(%mix, error = %e, "stopping the listen tap failed");
    }
}

/// The next listen frame to send: a listener more than [`MAX_BEHIND`] frames
/// behind (a slow connection) skips the older ones and goes on from the
/// newest, so it hears the band late by at most that many frames.
async fn next_frame(
    rx: &mut broadcast::Receiver<Bytes>,
) -> Result<Bytes, broadcast::error::RecvError> {
    loop {
        let frame = rx.recv().await?;
        if rx.len() <= MAX_BEHIND {
            return Ok(frame);
        }
    }
}

/// A session's subscriptions for the slot it listens to: the slot's own
/// frames and, for a `&hil=1` session only, the slot's listen probe behind
/// its gate.
pub struct Feeds {
    pub listen: broadcast::Receiver<Bytes>,
    pub probe: Option<broadcast::Receiver<Bytes>>,
    gate: ProbeGate,
}

/// The feeds of `slot` (`hil`: [`asks_for_probe`]); without `hil` there is
/// no probe receiver at all.
pub fn feeds(media: &MediaLink, slot: usize, hil: bool) -> Feeds {
    Feeds {
        listen: media.subscribe(slot),
        probe: hil.then(|| media.subscribe_probe(slot)),
        gate: ProbeGate::default(),
    }
}

/// What a session sends next.
#[derive(Debug)]
pub enum Out {
    /// A frame for the socket; `edge` (`probe` / `listening`) is the status
    /// sent before it at a burst's edge. A probe frame never counts in the
    /// listen diagnostics.
    Frame {
        data: Bytes,
        probe: bool,
        edge: Option<&'static str>,
    },
    /// Frames this many behind were lost (logged).
    Lagged(u64),
    /// The media link is gone.
    Closed,
}

impl Feeds {
    /// The next frame to send. Cancel-safe at its awaits (the session's
    /// `select!`): a frame taken is sent or dropped before the next await.
    pub async fn next(&mut self) -> Out {
        self.next_at(std::time::Instant::now).await
    }

    /// [`Self::next`] on the clock `now`, read once per frame taken. Probe
    /// frames first: they stand in for the slot's frames of the same moment.
    async fn next_at(&mut self, mut now: impl FnMut() -> std::time::Instant) -> Out {
        let Self {
            listen,
            probe,
            gate,
        } = self;
        loop {
            let probe_frame = async {
                match probe.as_mut() {
                    Some(rx) => next_frame(rx).await,
                    None => std::future::pending().await,
                }
            };
            let (from_probe, got) = tokio::select! {
                biased;
                got = probe_frame => (true, got),
                got = next_frame(listen) => (false, got),
            };
            let data = match got {
                Ok(data) => data,
                Err(broadcast::error::RecvError::Lagged(n)) => return Out::Lagged(n),
                Err(broadcast::error::RecvError::Closed) => return Out::Closed,
            };
            let edge = if from_probe {
                gate.probe(now()).then_some("probe")
            } else {
                match gate.listen(now()) {
                    Pass::Drop => continue,
                    Pass::Send => None,
                    Pass::SendLeaving => Some("listening"),
                }
            };
            return Out::Frame {
                data,
                probe: from_probe,
                edge,
            };
        }
    }
}

async fn session(mut socket: WebSocket, state: AppState, hil: bool) {
    tracing::info!(hil, "Audio WebSocket connected");
    let mut current: Option<(MixId, String)> = None;
    let mut feed: Option<Feeds> = None;
    let mut last_audio = Instant::now();
    let mut first_logged = false;
    loop {
        let listening = feed.is_some();
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::ListenStart { member_id }) => {
                        if let Some((mix, _)) = current.take() {
                            stop(&state, &mix).await;
                        }
                        feed = None;
                        let Some(page) = state.page(&member_id) else {
                            if socket.send(status("no_source", None)).await.is_err() {
                                break;
                            }
                            continue;
                        };
                        match start(&state, &page.mix).await {
                            Ok(()) => {
                                tracing::info!(target = %member_id, mix = %page.mix, hil, "Audio listen started");
                                feed = Some(feeds(&state.media, slot_of(&state, &page.mix), hil));
                                current = Some((page.mix.clone(), member_id.clone()));
                                last_audio = Instant::now();
                                if socket.send(status("listening", Some(member_id))).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                if !matches!(&e, EngineError::Refused(b) if b.code == ErrCode::NoSource) {
                                    tracing::warn!(error = %e, "listen start failed");
                                }
                                if socket.send(status("no_source", None)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(ClientMsg::ListenStop) => {
                        tracing::info!("Audio listen stopped");
                        if let Some((mix, _)) = current.take() {
                            stop(&state, &mix).await;
                        }
                        feed = None;
                        if socket.send(status("stopped", None)).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            out = async { match feed.as_mut() { Some(f) => f.next().await, None => std::future::pending().await } }, if listening => {
                match out {
                    Out::Frame { data, probe, edge } => {
                        last_audio = Instant::now();
                        if let Some(edge) = edge {
                            let target = current.as_ref().map(|(_, id)| id.clone());
                            tracing::info!(edge, target = ?target, "listen probe");
                            if socket.send(status(edge, target)).await.is_err() {
                                break;
                            }
                        }
                        if socket.send(Message::Binary(data)).await.is_err() {
                            break;
                        }
                        if !probe {
                            state.media.forwarded();
                        }
                        if !first_logged {
                            tracing::info!("first binary frame forwarded on /ws/audio");
                            first_logged = true;
                        }
                    }
                    Out::Lagged(n) => {
                        tracing::debug!(skipped = n, "listen frames skipped");
                    }
                    Out::Closed => break,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)), if listening => {
                if last_audio.elapsed() > Duration::from_secs(5) {
                    if socket.send(status("no_source", None)).await.is_err() {
                        break;
                    }
                    last_audio = Instant::now();
                }
            }
        }
    }
    if let Some((mix, _)) = current.take() {
        stop(&state, &mix).await;
    }
    tracing::info!("Audio WebSocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_engineer_mix_is_slot_zero_once_the_topology_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::site_view::tests::test_config(), dir.path());
        // No topology yet: every mix is taken as another mix's.
        assert_eq!(slot_of(&state, &MixId::new("engineer")), 1);
        assert_eq!(slot_of(&state, &MixId::new("member1")), 1);
    }

    /// `frames` frames queued for one listener.
    fn queued(frames: u8) -> broadcast::Receiver<bytes::Bytes> {
        let (tx, rx) = broadcast::channel(64);
        for k in 0..frames {
            tx.send(bytes::Bytes::from(vec![k])).unwrap();
        }
        rx
    }

    /// The frames `next_frame` hands out until none is left (each wait
    /// bounded: a skip that never ends fails here, not by hanging).
    async fn sent(rx: &mut broadcast::Receiver<bytes::Bytes>) -> Vec<u8> {
        let mut out = Vec::new();
        while !rx.is_empty() {
            let frame = tokio::time::timeout(Duration::from_secs(5), next_frame(rx))
                .await
                .expect("a frame within 5 s")
                .expect("an open channel");
            out.push(frame[0]);
        }
        out
    }

    #[tokio::test]
    async fn a_slow_listener_skips_to_the_newest_frames() {
        // 20 frames behind: the oldest are skipped, the newest five sent in order.
        let mut rx = queued(20);
        assert_eq!(sent(&mut rx).await, [15, 16, 17, 18, 19]);
        // Exactly MAX_BEHIND frames behind the one it takes: nothing skipped.
        let mut rx = queued(MAX_BEHIND as u8 + 1);
        assert_eq!(sent(&mut rx).await, [0, 1, 2, 3, 4]);
        // A lagged receiver reports it (the session logs and goes on).
        let (tx, mut rx) = broadcast::channel(2);
        for k in 0..3u8 {
            tx.send(bytes::Bytes::from(vec![k])).unwrap();
        }
        assert_eq!(
            next_frame(&mut rx).await,
            Err(broadcast::error::RecvError::Lagged(1))
        );
        assert_eq!(next_frame(&mut rx).await.unwrap()[0], 1);
        drop(tx);
        assert_eq!(rx.recv().await.unwrap()[0], 2);
        assert_eq!(
            next_frame(&mut rx).await,
            Err(broadcast::error::RecvError::Closed)
        );
    }

    #[test]
    fn the_query_reads_hil() {
        let query = |q: &str| {
            let uri: axum::http::Uri = format!("/ws/audio?{q}").parse().unwrap();
            Query::<WsQuery>::try_from_uri(&uri).unwrap().0
        };
        let q = query("token=t&hil=1");
        assert_eq!((q.token.as_deref(), q.hil), (Some("t"), Some(1)));
        assert_eq!(query("token=t").hil, None);
        assert_eq!(query("hil=0&token=t").hil, Some(0));
    }

    #[test]
    fn only_hil_1_asks_for_the_probe() {
        let query = |hil| WsQuery {
            token: Some("t".into()),
            proto: None,
            talk: None,
            hil,
        };
        assert!(asks_for_probe(&query(Some(1))));
        assert!(!asks_for_probe(&query(None)), "a plain listener");
        assert!(!asks_for_probe(&query(Some(0))));
        assert!(!asks_for_probe(&query(Some(2))));
    }

    #[test]
    fn a_socket_without_hil_holds_no_probe_feed() {
        let link = crate::engine::media::MediaLink::detached();
        for slot in [0, 1] {
            assert!(feeds(&link, slot, false).probe.is_none(), "slot {slot}");
            assert!(feeds(&link, slot, true).probe.is_some(), "slot {slot}");
        }
    }

    /// A feed on channels the test holds: (the slot's frames, the probe's, the feed).
    fn feed(hil: bool) -> (broadcast::Sender<Bytes>, broadcast::Sender<Bytes>, Feeds) {
        let (listen_tx, listen) = broadcast::channel(64);
        let (probe_tx, probe) = broadcast::channel(64);
        let feed = Feeds {
            listen,
            probe: hil.then_some(probe),
            gate: ProbeGate::default(),
        };
        (listen_tx, probe_tx, feed)
    }

    /// What `f` sends next (bounded: a feed that never answers fails here).
    async fn next(f: &mut Feeds, now: impl FnMut() -> std::time::Instant) -> Out {
        tokio::time::timeout(Duration::from_secs(5), f.next_at(now))
            .await
            .expect("an answer within 5 s")
    }

    /// A frame as (its one byte, a probe frame, the edge before it).
    fn frame(out: Out) -> (u8, bool, Option<&'static str>) {
        match out {
            Out::Frame { data, probe, edge } => (data[0], probe, edge),
            other => panic!("not a frame: {other:?}"),
        }
    }

    fn byte(b: u8) -> Bytes {
        Bytes::from(vec![b])
    }

    #[tokio::test]
    async fn a_hil_feed_sends_the_probe_in_place_of_the_slots_frames_and_names_its_edges() {
        let (listen_tx, probe_tx, mut f) = feed(true);
        let t0 = std::time::Instant::now();
        // One reading of the clock per frame taken.
        let mut clock = [0, 10, 30, 129, 130, 131]
            .map(|ms| t0 + Duration::from_millis(ms))
            .into_iter();
        let mut now = move || clock.next().expect("a time per frame");
        listen_tx.send(byte(1)).unwrap();
        assert_eq!(
            frame(next(&mut f, &mut now).await),
            (1, false, None),
            "no probe yet"
        );
        // A burst: probe frames, with the slot's own frames queued beside them.
        for b in [10, 11] {
            probe_tx.send(byte(b)).unwrap();
        }
        for b in [2, 3, 4] {
            listen_tx.send(byte(b)).unwrap();
        }
        assert_eq!(
            frame(next(&mut f, &mut now).await),
            (10, true, Some("probe")),
            "the first probe frame opens the burst"
        );
        assert_eq!(frame(next(&mut f, &mut now).await), (11, true, None));
        // Frame 2, 99 ms after the last probe frame, is dropped; frame 3, at
        // PROBE_HOLD, leaves the burst.
        assert_eq!(
            frame(next(&mut f, &mut now).await),
            (3, false, Some("listening"))
        );
        assert_eq!(frame(next(&mut f, &mut now).await), (4, false, None));
    }

    #[tokio::test]
    async fn a_plain_feed_sends_the_slots_frames_and_reports_lag_and_the_end() {
        let (listen_tx, probe_tx, mut f) = feed(false);
        assert!(probe_tx.send(byte(10)).is_err(), "nobody holds the probe");
        let t0 = std::time::Instant::now();
        let mut now = || t0;
        for b in [1, 2] {
            listen_tx.send(byte(b)).unwrap();
        }
        assert_eq!(frame(next(&mut f, &mut now).await), (1, false, None));
        assert_eq!(frame(next(&mut f, &mut now).await), (2, false, None));
        // 66 frames into a channel of 64: two lost, then the newest five.
        for b in 0..66 {
            listen_tx.send(byte(b)).unwrap();
        }
        assert!(matches!(next(&mut f, &mut now).await, Out::Lagged(2)));
        assert_eq!(frame(next(&mut f, &mut now).await), (61, false, None));
        drop(listen_tx);
        for b in 62..66 {
            assert_eq!(frame(next(&mut f, &mut now).await), (b, false, None));
        }
        assert!(matches!(next(&mut f, &mut now).await, Out::Closed));
    }

    /// Listen taps against the real engine (NullRt).
    #[cfg(unix)]
    mod live {
        use super::*;
        use crate::engine::testkit::{EngineHarness, wait_until};
        use iem_engine_proto::InputId;

        /// A frame one of the two listeners got.
        struct Got {
            hil: bool,
            probe: bool,
            edge: Option<&'static str>,
            data: Bytes,
        }

        fn got(hil: bool, out: Out) -> Option<Got> {
            match out {
                Out::Frame { data, probe, edge } => Some(Got {
                    hil,
                    probe,
                    edge,
                    data,
                }),
                Out::Lagged(_) => None,
                Out::Closed => panic!("the media link closed"),
            }
        }

        /// The decoded frame's peak.
        fn peak(dec: &mut opus::Decoder, packet: &[u8]) -> f32 {
            let mut out = [0f32; 2 * iem_engine_proto::FRAME_48K];
            let n = dec.decode_float(packet, &mut out, false).unwrap();
            assert_eq!(n, iem_engine_proto::FRAME_48K);
            out.iter().fold(0f32, |m, x| m.max(x.abs()))
        }

        fn decoder() -> opus::Decoder {
            opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap()
        }

        /// The owner's rule (#9 2026-09-28) for the listen probe: a socket
        /// without `&hil=1` (any socket a member could hear) never gets a
        /// probe frame; the engineer's `&hil=1` socket gets the burst's
        /// tone in place of the slot's own frames.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_listener_without_hil_gets_only_silence_while_a_probe_runs_and_a_hil_listener_gets_the_tone()
         {
            let h = EngineHarness::start_with(|c| c.flags.test_signal = true);
            let (_d, s) = h.state().await;
            let media = s.media.clone();
            wait_until("the media pipe", || media.connected()).await;
            let m1 = MixId::new("member1");
            start(&s, &m1).await.unwrap();
            let slot = slot_of(&s, &m1);
            assert_eq!(slot, 1);
            let mut plain = feeds(&s.media, slot, false);
            let mut hil = feeds(&s.media, slot, true);
            // The tap runs, so the stream and its start fade-in do.
            let first = tokio::time::timeout(Duration::from_secs(5), plain.next())
                .await
                .expect("a frame of member1's tap within 5 s");
            assert!(matches!(
                first,
                Out::Frame {
                    probe: false,
                    edge: None,
                    ..
                }
            ));
            h.supervise(Cmd::HilTestSignal {
                input: InputId::new("mic1"),
                hz: 1000.0,
                dbfs: -20.0,
                ttl_s: 1.0,
                card_tx: vec![94, 95],
                listen: true,
            })
            .expect("the engine runs the listen probe");
            // Both listeners' frames in the order they came, until the burst
            // (1 s) is over and the hil listener was told so; ≤ 10 s.
            let mut all: Vec<Got> = Vec::new();
            let ended = |all: &[Got]| {
                all.iter().rev().filter(|g| g.hil).find_map(|g| g.edge) == Some("listening")
            };
            let t0 = std::time::Instant::now();
            while t0.elapsed() < Duration::from_millis(1500) || !ended(&all) {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "the hil listener heard a burst begin and end within 10 s"
                );
                tokio::select! {
                    out = plain.next() => all.extend(got(false, out)),
                    out = hil.next() => all.extend(got(true, out)),
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
            let begun = all
                .iter()
                .position(|g| g.hil && g.edge == Some("probe"))
                .expect("the hil listener was told `probe`");
            let over = all
                .iter()
                .rposition(|g| g.hil && g.edge == Some("listening"))
                .expect("and then `listening`");
            // The plain listener: the slot's own frames only, silent, and
            // they went on while the probe ran.
            let mut dec = decoder();
            let mut during = 0;
            for (k, g) in all.iter().enumerate().filter(|(_, g)| !g.hil) {
                assert!(
                    !g.probe && g.edge.is_none(),
                    "frame {k}: a probe frame or an edge"
                );
                let p = peak(&mut dec, &g.data);
                assert!(p < 1e-4, "frame {k} of the plain listener peaks at {p}");
                during += usize::from((begun..over).contains(&k));
            }
            assert!(during >= 10, "{during} plain frames while the probe ran");
            // The hil listener: probe frames exactly inside its bursts, the
            // tone at −20 dBFS; the slot's own frames outside them, silent.
            let (mut probe_dec, mut own_dec) = (decoder(), decoder());
            let (mut in_burst, mut probes, mut loudest) = (false, 0, 0f32);
            for g in all.iter().filter(|g| g.hil) {
                match g.edge {
                    Some("probe") => in_burst = true,
                    Some("listening") => in_burst = false,
                    Some(other) => panic!("an unknown edge {other}"),
                    None => {}
                }
                assert_eq!(g.probe, in_burst, "a probe frame exactly inside a burst");
                if g.probe {
                    probes += 1;
                    loudest = loudest.max(peak(&mut probe_dec, &g.data));
                } else {
                    let p = peak(&mut own_dec, &g.data);
                    assert!(p < 1e-4, "the slot's own frame peaks at {p}");
                }
            }
            assert!(probes >= 10, "{probes} probe frames");
            let dbfs = 20.0 * loudest.log10();
            assert!(
                (dbfs + 20.0).abs() <= 1.0,
                "the probe's tone at {dbfs} dBFS"
            );
        }

        fn listeners(state: &AppState, mix: &MixId) -> Option<usize> {
            state
                .listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(mix)
                .copied()
        }

        /// The engine's listen slots as the mirror has them.
        fn heard(state: &AppState) -> [Option<MixId>; 2] {
            state.engine.mirror().transient.listen.clone()
        }

        #[tokio::test]
        async fn listeners_share_a_tap_and_the_last_one_stops_it() {
            let h = EngineHarness::start();
            let (_d, s) = h.state().await;
            let m1 = MixId::new("member1");
            assert_eq!(slot_of(&s, &MixId::new("engineer")), 0);
            assert_eq!(slot_of(&s, &m1), 1);

            start(&s, &m1).await.unwrap();
            start(&s, &m1).await.unwrap();
            assert_eq!(listeners(&s, &m1), Some(2));
            wait_until("the member tap", || heard(&s)[1] == Some(m1.clone())).await;

            stop(&s, &m1).await;
            assert_eq!(listeners(&s, &m1), Some(1));
            // A later request is in the mirror, so a stop sent before it would show.
            let later = Cmd::SetMix {
                mix: m1.clone(),
                volume_db: Some(-1.0),
                muted: None,
            };
            s.engine.request_applied(later, None).await.unwrap();
            assert_eq!(heard(&s)[1], Some(m1.clone()), "one listener is left");

            stop(&s, &m1).await;
            assert_eq!(listeners(&s, &m1), None);
            wait_until("the tap to stop", || heard(&s)[1].is_none()).await;
        }
    }
}
