//! The engine's media pipe from the server (X3–X5; S5 design note §4): the
//! engine's two listen taps arrive as 20 ms 48 kHz stereo frames (stream 0
//! the engineer's, stream 1 one other mix's) and are Opus-encoded here —
//! CELT only (`RESTRICTED_LOWDELAY`), no FEC — for `/ws/audio`; talkback
//! goes the other way as 20 ms 48 kHz mono frames on stream 16. The engine
//! links no codec (I1).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use iem_engine_proto::media::stream;
use iem_engine_proto::{FRAME_48K, MediaHeader};
use tokio::sync::{broadcast, mpsc};

use super::wire;

/// Opus bitrate of the listen stream (stereo).
pub const LISTEN_BITRATE: i32 = 128_000;
/// Talkback frames waiting for the pipe at most (then the newest is dropped).
pub const TALK_QUEUE: usize = 16;

/// Listen-pipeline health (`GET /api/audio/diagnostics`, F28): the
/// predecessor's field names, now fed by the engine's media pipe.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct AudioDiagnostics {
    /// Frames arrived from the engine in the last 2 s
    pub receiving_oiem: bool,
    /// The same, under the predecessor's second name
    pub receiving_vban: bool,
    pub packets_per_second: f32,
    pub opus_frames_per_second: f32,
    pub last_frame_size_bytes: usize,
    /// Peak of the last frame (dBFS)
    pub peak_db: f32,
    pub last_sequence: u16,
    pub sequence_gaps: u64,
    /// Opus frames sent to listeners since start
    pub frames_forwarded: u64,
}

/// The Opus encoder of one listen tap.
pub struct ListenEncoder(opus::Encoder);

impl ListenEncoder {
    pub fn new() -> Result<Self, opus::Error> {
        let mut e =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay)?;
        e.set_bitrate(opus::Bitrate::Bits(LISTEN_BITRATE))?;
        e.set_inband_fec(false)?;
        Ok(Self(e))
    }

    /// One interleaved stereo frame of 960 samples per channel.
    pub fn encode(&mut self, interleaved: &[f32]) -> Result<Vec<u8>, opus::Error> {
        let mut buf = [0u8; 4000];
        let n = self.0.encode_float(interleaved, &mut buf)?;
        Ok(buf.get(..n).unwrap_or_default().to_vec())
    }

    pub fn application(&mut self) -> Result<opus::Application, opus::Error> {
        self.0.get_application()
    }
}

/// Peak of a frame in dBFS (−150 when silent).
pub fn peak_db(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 1e-7 {
        20.0 * peak.log10()
    } else {
        -150.0
    }
}

#[derive(Debug)]
struct Stats {
    last_frame: Option<Instant>,
    window_start: Instant,
    in_window: u32,
    per_second: f32,
    last_size: usize,
    peak_db: f32,
    last_seq: [Option<u64>; 2],
    gaps: u64,
}

impl Stats {
    fn new() -> Self {
        Self {
            last_frame: None,
            window_start: Instant::now(),
            in_window: 0,
            per_second: 0.0,
            last_size: 0,
            peak_db: -150.0,
            last_seq: [None, None],
            gaps: 0,
        }
    }

    fn frame(&mut self, slot: usize, seq: u64, size: usize, peak_db: f32, now: Instant) {
        self.last_frame = Some(now);
        self.in_window += 1;
        let elapsed = now
            .saturating_duration_since(self.window_start)
            .as_secs_f32();
        if elapsed >= 1.0 {
            self.per_second = self.in_window as f32 / elapsed;
            self.in_window = 0;
            self.window_start = now;
        }
        if let Some(Some(last)) = self.last_seq.get(slot)
            && seq > last + 1
        {
            self.gaps += seq - last - 1;
        }
        if let Some(s) = self.last_seq.get_mut(slot) {
            *s = Some(seq);
        }
        self.last_size = size;
        self.peak_db = peak_db;
    }
}

struct Inner {
    pipe: String,
    listen: [broadcast::Sender<Bytes>; 2],
    talk_tx: mpsc::Sender<Vec<f32>>,
    talk_rx: Mutex<Option<mpsc::Receiver<Vec<f32>>>>,
    stats: Mutex<Stats>,
    connected: AtomicBool,
    forwarded: AtomicU64,
    talk_dropped: AtomicU64,
}

/// A handle on the media pipe (cheap to clone).
#[derive(Clone)]
pub struct MediaLink {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MediaLink {
    /// A link that never connects (tests, and before start-up).
    pub fn detached() -> Self {
        let (talk_tx, talk_rx) = mpsc::channel(TALK_QUEUE);
        Self {
            inner: Arc::new(Inner {
                pipe: String::new(),
                listen: [broadcast::channel(64).0, broadcast::channel(64).0],
                talk_tx,
                talk_rx: Mutex::new(Some(talk_rx)),
                stats: Mutex::new(Stats::new()),
                connected: AtomicBool::new(false),
                forwarded: AtomicU64::new(0),
                talk_dropped: AtomicU64::new(0),
            }),
        }
    }

    /// Connects to `<pipe>.media` and keeps it connected.
    pub fn spawn(pipe: String) -> Self {
        let mut link = Self::detached();
        if let Some(inner) = Arc::get_mut(&mut link.inner) {
            inner.pipe = pipe;
        }
        let task = link.clone();
        tokio::spawn(async move { task.run().await });
        link
    }

    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    pub fn pipe(&self) -> &str {
        &self.inner.pipe
    }

    /// Opus frames of listen slot 0 (the engineer's tap) or 1.
    pub fn subscribe(&self, slot: usize) -> broadcast::Receiver<Bytes> {
        self.inner.listen[slot.min(1)].subscribe()
    }

    /// Counts a frame a listener forwarded.
    pub fn forwarded(&self) {
        self.inner.forwarded.fetch_add(1, Ordering::Relaxed);
    }

    /// Queues one talkback frame for the engine; `false` when it was dropped.
    pub fn send_talkback(&self, frame: Vec<f32>) -> bool {
        let ok = self.inner.talk_tx.try_send(frame).is_ok();
        if !ok {
            self.inner.talk_dropped.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }

    pub fn diagnostics(&self) -> AudioDiagnostics {
        let s = lock(&self.inner.stats);
        let receiving = s
            .last_frame
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
        AudioDiagnostics {
            receiving_oiem: receiving,
            receiving_vban: receiving,
            packets_per_second: s.per_second,
            opus_frames_per_second: s.per_second,
            last_frame_size_bytes: s.last_size,
            peak_db: s.peak_db,
            last_sequence: s.last_seq.iter().flatten().max().map_or(0, |q| *q as u16),
            sequence_gaps: s.gaps,
            frames_forwarded: self.inner.forwarded.load(Ordering::Relaxed),
        }
    }

    /// One listen frame from the engine: encoded and sent to the slot's listeners.
    fn listen_frame(&self, enc: &mut [Option<ListenEncoder>; 2], h: &MediaHeader, samples: &[f32]) {
        let slot = match h.stream {
            stream::ENGINEER_LISTEN => 0,
            stream::MEMBER_LISTEN => 1,
            _ => return,
        };
        if h.channels != 2 || usize::from(h.frames) != FRAME_48K {
            tracing::warn!(
                stream = h.stream,
                channels = h.channels,
                frames = h.frames,
                "odd listen frame skipped"
            );
            return;
        }
        let Some(Some(e)) = enc.get_mut(slot) else {
            return;
        };
        let packet = match e.encode(samples) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(error = %err, "Opus encoding failed");
                return;
            }
        };
        lock(&self.inner.stats).frame(slot, h.seq, packet.len(), peak_db(samples), Instant::now());
        let _ = self.inner.listen[slot].send(Bytes::from(packet));
    }

    async fn run(&self) {
        let Some(mut talk_rx) = lock(&self.inner.talk_rx).take() else {
            return;
        };
        let mut enc: [Option<ListenEncoder>; 2] = [
            ListenEncoder::new()
                .map_err(|e| tracing::error!(error = %e, "no Opus encoder"))
                .ok(),
            ListenEncoder::new().ok(),
        ];
        loop {
            match self.connect().await {
                Ok((mut r, mut w)) => {
                    tracing::info!(pipe = %self.inner.pipe, "media pipe connected");
                    self.inner.connected.store(true, Ordering::Release);
                    let mut seq = 0u64;
                    loop {
                        tokio::select! {
                            frame = wire::read_media_frame(&mut r) => match frame {
                                Ok((h, samples)) => self.listen_frame(&mut enc, &h, &samples),
                                Err(e) => {
                                    tracing::warn!(error = %e, "media pipe closed");
                                    break;
                                }
                            },
                            Some(frame) = talk_rx.recv() => {
                                let h = MediaHeader {
                                    stream: stream::TALKBACK,
                                    channels: 1,
                                    seq,
                                    frames: u16::try_from(frame.len()).unwrap_or(0),
                                };
                                seq += 1;
                                if let Err(e) = wire::write_media_frame(&mut w, &h, &frame).await {
                                    tracing::warn!(error = %e, "talkback write failed");
                                    break;
                                }
                            }
                        }
                    }
                    self.inner.connected.store(false, Ordering::Release);
                }
                Err(e) => tracing::debug!(error = %e, "media pipe not reachable"),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn connect(&self) -> std::io::Result<(super::client::Reader, super::client::Writer)> {
        wire::connect(&wire::media_pipe(&self.inner.pipe)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo tone frame: `hz` at `amp`, frame `k` of a continuous signal.
    pub(crate) fn tone(hz: f64, amp: f32, k: usize) -> Vec<f32> {
        (0..FRAME_48K)
            .flat_map(|i| {
                let n = (k * FRAME_48K + i) as f64;
                let x = amp * (std::f64::consts::TAU * hz * n / 48_000.0).sin() as f32;
                [x, x]
            })
            .collect()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn listen_frames_are_celt_opus_that_decode_to_the_tone() {
        let mut enc = ListenEncoder::new().unwrap();
        assert_eq!(enc.application().unwrap(), opus::Application::LowDelay);
        let mut dec = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
        let mut last = vec![0f32; 2 * FRAME_48K];
        for k in 0..10 {
            let packet = enc.encode(&tone(1000.0, 0.5, k)).unwrap();
            assert!(
                !packet.is_empty() && packet.len() < 1500,
                "{}",
                packet.len()
            );
            let n = dec.decode_float(&packet, &mut last, false).unwrap();
            assert_eq!(n, FRAME_48K);
        }
        let left: Vec<f32> = last.iter().step_by(2).copied().collect();
        let rms = (left.iter().map(|x| x * x).sum::<f32>() / left.len() as f32).sqrt();
        let want = 0.5 / std::f32::consts::SQRT_2;
        assert!((20.0 * (rms / want).log10()).abs() < 1.0, "rms {rms}");
        let c = crossings(&left);
        assert!(
            (38..=42).contains(&c),
            "1 kHz over 20 ms: {c} zero crossings"
        );
        assert!(
            enc.encode(&[0.0; 10]).is_err(),
            "a frame must be 960 samples per channel"
        );
    }

    #[test]
    fn peaks_are_in_dbfs() {
        assert_eq!(peak_db(&[0.0, 0.0]), -150.0);
        assert!((peak_db(&[0.5, -1.0]) - 0.0).abs() < 1e-6);
        assert!((peak_db(&[0.1]) + 20.0).abs() < 1e-4);
    }

    #[tokio::test]
    async fn frames_reach_their_slot_and_count_in_the_diagnostics() {
        let link = MediaLink::detached();
        let mut enc = [ListenEncoder::new().ok(), ListenEncoder::new().ok()];
        let mut rx0 = link.subscribe(0);
        let mut rx1 = link.subscribe(1);
        let h = |stream: u8, seq: u64| MediaHeader {
            stream,
            channels: 2,
            seq,
            frames: 960,
        };
        link.listen_frame(
            &mut enc,
            &h(stream::ENGINEER_LISTEN, 0),
            &tone(440.0, 0.1, 0),
        );
        link.listen_frame(&mut enc, &h(stream::MEMBER_LISTEN, 5), &tone(440.0, 0.1, 0));
        link.listen_frame(&mut enc, &h(stream::MEMBER_LISTEN, 8), &tone(440.0, 0.1, 1));
        // Not a listen stream, or the wrong shape: skipped.
        link.listen_frame(&mut enc, &h(stream::TALKBACK, 9), &tone(440.0, 0.1, 0));
        let mono = MediaHeader {
            channels: 1,
            ..h(stream::ENGINEER_LISTEN, 1)
        };
        link.listen_frame(&mut enc, &mono, &[0.0; 960]);
        assert!(!rx0.recv().await.unwrap().is_empty());
        assert!(rx0.try_recv().is_err());
        rx1.recv().await.unwrap();
        rx1.recv().await.unwrap();
        link.forwarded();
        let d = link.diagnostics();
        assert!(d.receiving_oiem && d.receiving_vban);
        assert_eq!(d.sequence_gaps, 2, "5 → 8 on stream 1 skipped two");
        assert_eq!(d.last_sequence, 8);
        assert_eq!(d.frames_forwarded, 1);
        assert!((d.peak_db + 20.0).abs() < 0.1, "{}", d.peak_db);
        assert!(d.last_frame_size_bytes > 0);
        assert!(!link.connected());
        assert_eq!(MediaLink::detached().diagnostics().receiving_oiem, false);
    }

    #[tokio::test]
    async fn talkback_frames_queue_until_full() {
        let link = MediaLink::detached();
        for _ in 0..TALK_QUEUE {
            assert!(link.send_talkback(vec![0.0; 960]));
        }
        assert!(!link.send_talkback(vec![0.0; 960]), "full: dropped");
        assert_eq!(link.inner.talk_dropped.load(Ordering::Relaxed), 1);
    }
}
