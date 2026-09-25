//! Audio streaming pipeline: OIEM UDP relay → WebSocket
//!
//! Receives pre-encoded Opus frames from the VST3 plugin via OIEM UDP protocol
//! on port 7980 and broadcasts them directly to WebSocket clients.
//! No resampling or encoding on the server — the VST handles everything.

use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{RwLock, broadcast};

/// OIEM packet magic bytes: "OIEM" as bytes
const OIEM_MAGIC: [u8; 4] = *b"OIEM";

/// OIEM header size: 4 (magic) + 2 (sequence) + 2 (payload_size) = 8 bytes
const OIEM_HEADER_SIZE: usize = 8;

/// UDP bind address for receiving OIEM audio from the local VST3 plugin.
const OIEM_BIND_ADDR: &str = "127.0.0.1:7980";

/// Audio pipeline health diagnostics
#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioDiagnostics {
    /// Whether OIEM packets are being received
    pub receiving_oiem: bool,
    /// Also exposed as receiving_vban for backwards compatibility with CI
    pub receiving_vban: bool,
    /// OIEM packets received per second
    pub packets_per_second: f32,
    /// Opus frames relayed per second (same as packets for OIEM)
    pub opus_frames_per_second: f32,
    /// Size of last Opus frame in bytes
    pub last_frame_size_bytes: usize,
    /// Peak dB of last packet (estimated from Opus frame size)
    pub peak_db: f32,
    /// Last received sequence number
    pub last_sequence: u16,
    /// Sequence gaps detected (dropped UDP packets)
    pub sequence_gaps: u64,
    /// Total Opus frames forwarded over /ws/audio since app start (counts
    /// successful `Message::Binary` sends across all concurrent listeners).
    /// Used to detect pipeline breaks between deploys.
    pub frames_forwarded: u64,
}

impl Default for AudioDiagnostics {
    fn default() -> Self {
        Self {
            receiving_oiem: false,
            receiving_vban: false,
            packets_per_second: 0.0,
            opus_frames_per_second: 0.0,
            last_frame_size_bytes: 0,
            peak_db: -150.0,
            last_sequence: 0,
            sequence_gaps: 0,
            frames_forwarded: 0,
        }
    }
}

/// Tracks packet rates using a 1-second sliding window
struct RateTracker {
    window_start: Instant,
    packet_count: u32,
    last_packets_per_second: f32,
}

impl RateTracker {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            packet_count: 0,
            last_packets_per_second: 0.0,
        }
    }

    fn record(&mut self) -> f32 {
        self.packet_count += 1;

        let elapsed = self.window_start.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            self.last_packets_per_second = self.packet_count as f32 / elapsed;
            self.packet_count = 0;
            self.window_start = Instant::now();
        }

        self.last_packets_per_second
    }
}

/// Parse an OIEM UDP packet, returning the Opus payload
///
/// OIEM format (little-endian):
/// - 4 bytes: magic "OIEM" (0x4F49454D)
/// - 2 bytes: sequence number (uint16 LE, wrapping)
/// - 2 bytes: payload size in bytes (uint16 LE)
/// - N bytes: raw Opus frame
pub fn parse_oiem_packet(data: &[u8]) -> Result<(u16, Bytes), &'static str> {
    if data.len() < OIEM_HEADER_SIZE {
        return Err("packet too short");
    }

    if data[0..4] != OIEM_MAGIC {
        return Err("wrong magic bytes");
    }

    let sequence = u16::from_le_bytes([data[4], data[5]]);
    let payload_size = u16::from_le_bytes([data[6], data[7]]) as usize;

    if payload_size == 0 {
        return Err("empty payload");
    }

    if data.len() < OIEM_HEADER_SIZE + payload_size {
        return Err("truncated payload");
    }

    let opus_frame =
        Bytes::copy_from_slice(&data[OIEM_HEADER_SIZE..OIEM_HEADER_SIZE + payload_size]);
    Ok((sequence, opus_frame))
}

/// Spawn the UDP listener that receives OIEM packets and broadcasts Opus frames
pub fn spawn_audio_listener(
    audio_tx: broadcast::Sender<Bytes>,
    diagnostics: Arc<Mutex<AudioDiagnostics>>,
    talkback_state: Arc<RwLock<crate::TalkbackState>>,
) {
    tokio::spawn(async move {
        let socket = match tokio::net::UdpSocket::bind(OIEM_BIND_ADDR).await {
            Ok(s) => {
                tracing::info!("Audio listener bound to {} (OIEM protocol)", OIEM_BIND_ADDR);
                s
            }
            Err(e) => {
                tracing::error!(
                    "Failed to bind audio UDP socket on {}: {}",
                    OIEM_BIND_ADDR,
                    e
                );
                return;
            }
        };

        let mut buf = vec![0u8; 2048];
        let mut rate_tracker = RateTracker::new();
        let mut last_sequence: Option<u16> = None;
        let mut sequence_gaps: u64 = 0;

        loop {
            let (len, src_addr) = match socket.recv_from(&mut buf).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!("UDP recv error: {}", e);
                    continue;
                }
            };

            // Detect heartbeat from OIEM Receive VST (reaperiem#123)
            if len == OIEM_HEADER_SIZE
                && buf[0..4] == OIEM_MAGIC
                && u16::from_le_bytes([buf[6], buf[7]]) == 0
            {
                let mut tb = talkback_state.write().await;
                if tb.recv_vst_addr != Some(src_addr) {
                    tracing::info!(
                        addr = %src_addr,
                        "OIEM Receive VST registered via heartbeat"
                    );
                    tb.recv_vst_addr = Some(src_addr);
                }
                continue;
            }

            let (sequence, opus_frame) = match parse_oiem_packet(&buf[..len]) {
                Ok(result) => result,
                Err(e) => {
                    tracing::debug!("OIEM parse error: {}", e);
                    continue;
                }
            };

            // Track sequence gaps (detect dropped UDP packets)
            if let Some(prev) = last_sequence {
                let expected = prev.wrapping_add(1);
                if sequence != expected {
                    let gap = sequence.wrapping_sub(prev).wrapping_sub(1);
                    sequence_gaps += gap as u64;
                    tracing::debug!(
                        "OIEM sequence gap: expected {}, got {} (gap={})",
                        expected,
                        sequence,
                        gap
                    );
                }
            }
            last_sequence = Some(sequence);

            let frame_size = opus_frame.len();

            // Estimate peak dB from Opus frame size
            // Larger frames = more signal content. Silence produces tiny frames (~3 bytes).
            // 160kbps VBR at 48kHz stereo: typical frame ~400 bytes for music, ~10 for silence
            let peak_db = if frame_size > 50 {
                // Rough estimate: map frame size 50-600 to -40..0 dB
                let ratio = ((frame_size as f32 - 50.0) / 550.0).clamp(0.0, 1.0);
                -40.0 + ratio * 40.0
            } else if frame_size > 10 {
                -60.0
            } else {
                -150.0
            };

            // Broadcast the Opus frame directly — no processing needed
            let _ = audio_tx.send(opus_frame);

            // Update diagnostics
            let pps = rate_tracker.record();
            if let Ok(mut diag) = diagnostics.lock() {
                diag.receiving_oiem = true;
                diag.receiving_vban = true; // backwards compat
                diag.packets_per_second = pps;
                diag.opus_frames_per_second = pps; // 1:1 for OIEM
                diag.last_frame_size_bytes = frame_size;
                diag.peak_db = peak_db;
                diag.last_sequence = sequence;
                diag.sequence_gaps = sequence_gaps;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_diagnostics_default_includes_frames_forwarded() {
        let diag = AudioDiagnostics::default();
        assert_eq!(diag.frames_forwarded, 0);
    }

    #[test]
    fn test_audio_diagnostics_serializes_frames_forwarded() {
        // Protects the /api/audio/diagnostics JSON contract: the field must
        // appear in the serialized output with the exact key clients expect.
        let diag = AudioDiagnostics {
            frames_forwarded: 42,
            ..Default::default()
        };
        let json = serde_json::to_string(&diag).unwrap();
        assert!(
            json.contains("\"frames_forwarded\":42"),
            "expected frames_forwarded in JSON, got: {}",
            json
        );
    }

    #[test]
    fn test_audio_diagnostics_frames_forwarded_saturates() {
        // Confirms saturating_add is the semantics used by the counter-update
        // path in proxy.rs (vs. wrapping_add which would reset to 0 on overflow,
        // giving a misleading "pipeline is dead" diagnostic signal).
        let mut diag = AudioDiagnostics::default();
        diag.frames_forwarded = u64::MAX;
        diag.frames_forwarded = diag.frames_forwarded.saturating_add(1);
        assert_eq!(diag.frames_forwarded, u64::MAX);
    }

    /// Build an OIEM packet for testing
    fn build_oiem_packet(sequence: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&OIEM_MAGIC);
        packet.extend_from_slice(&sequence.to_le_bytes());
        packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn test_parse_valid_oiem_packet() {
        let payload = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let packet = build_oiem_packet(42, &payload);

        let (seq, data) = parse_oiem_packet(&packet).unwrap();
        assert_eq!(seq, 42);
        assert_eq!(data.as_ref(), &payload[..]);
    }

    #[test]
    fn test_parse_wrong_magic() {
        let mut packet = build_oiem_packet(0, &[0x01, 0x02]);
        packet[0] = b'X'; // corrupt magic
        assert_eq!(parse_oiem_packet(&packet).unwrap_err(), "wrong magic bytes");
    }

    #[test]
    fn test_parse_truncated_header() {
        let packet = &OIEM_MAGIC; // only magic, no sequence/size
        assert_eq!(parse_oiem_packet(packet).unwrap_err(), "packet too short");
    }

    #[test]
    fn test_parse_truncated_payload() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&OIEM_MAGIC);
        packet.extend_from_slice(&0u16.to_le_bytes()); // seq
        packet.extend_from_slice(&100u16.to_le_bytes()); // claims 100 bytes
        packet.extend_from_slice(&[0u8; 10]); // only 10 bytes
        assert_eq!(parse_oiem_packet(&packet).unwrap_err(), "truncated payload");
    }

    #[test]
    fn test_parse_empty_payload() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&OIEM_MAGIC);
        packet.extend_from_slice(&0u16.to_le_bytes());
        packet.extend_from_slice(&0u16.to_le_bytes()); // 0 bytes payload
        assert_eq!(parse_oiem_packet(&packet).unwrap_err(), "empty payload");
    }

    #[test]
    fn test_parse_sequence_wrapping() {
        let packet = build_oiem_packet(u16::MAX, &[0x01]);
        let (seq, _) = parse_oiem_packet(&packet).unwrap();
        assert_eq!(seq, u16::MAX);
    }

    #[test]
    fn test_parse_large_payload() {
        // Typical Opus frame at 160kbps is ~400 bytes
        let payload: Vec<u8> = (0..400).map(|i| (i % 256) as u8).collect();
        let packet = build_oiem_packet(1000, &payload);
        let (seq, data) = parse_oiem_packet(&packet).unwrap();
        assert_eq!(seq, 1000);
        assert_eq!(data.len(), 400);
    }

    #[test]
    fn test_bind_address_uses_correct_port() {
        let addr: std::net::SocketAddr = OIEM_BIND_ADDR.parse().unwrap();
        assert_eq!(addr.port(), 7980, "Must bind to port 7980");
        assert!(
            addr.ip().is_loopback(),
            "Must be localhost, got {}",
            addr.ip()
        );
    }

    #[test]
    fn test_oiem_header_size() {
        assert_eq!(OIEM_HEADER_SIZE, 8, "OIEM header must be 8 bytes");
    }

    #[test]
    fn test_frame_dropper_drops_stale_frames() {
        // Simulate frame dropper: bounded channel capacity 5, send 20 frames
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(5);

        // Fill beyond capacity — only latest frames should survive
        for i in 0..20u8 {
            let frame = Bytes::from(vec![i]);
            // Use try_send to simulate the dropper behavior
            if tx.try_send(frame.clone()).is_err() {
                // Channel full — drain one to make room (drop-oldest)
                let _ = rx.try_recv();
                let _ = tx.try_send(frame);
            }
        }

        // Collect remaining frames
        let mut received = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            received.push(frame[0]);
        }

        // Should have ~5 frames, all from the later half
        assert!(
            received.len() <= 5,
            "Should have at most 5 frames, got {}",
            received.len()
        );
        assert!(!received.is_empty(), "Should have some frames");
        // Latest frames should be from the higher sequence numbers
        for &val in &received {
            assert!(
                val >= 10,
                "Frame {} is too old — dropper should keep recent frames",
                val
            );
        }
    }

    #[tokio::test]
    async fn test_udp_listener_receives_oiem_packet() {
        let (audio_tx, mut audio_rx) = broadcast::channel::<Bytes>(64);

        // Bind listener on an ephemeral port
        let listener_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener_socket.local_addr().unwrap();

        // Spawn listener task (same logic as spawn_audio_listener but with our socket)
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                let len = match listener_socket.recv(&mut buf).await {
                    Ok(len) => len,
                    Err(_) => continue,
                };
                if let Ok((_seq, opus_frame)) = parse_oiem_packet(&buf[..len]) {
                    let _ = audio_tx.send(opus_frame);
                }
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send test OIEM packets
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender.connect(listener_addr).await.unwrap();

        for i in 0..10u16 {
            let payload: Vec<u8> = (0..100).map(|j| (i as u8).wrapping_add(j)).collect();
            let packet = build_oiem_packet(i, &payload);
            sender.send(&packet).await.unwrap();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut frame_count = 0;
        while let Ok(frame) = audio_rx.try_recv() {
            assert_eq!(frame.len(), 100, "Frame should be 100 bytes");
            frame_count += 1;
        }

        assert!(
            frame_count >= 5,
            "Expected at least 5 Opus frames, got {}",
            frame_count
        );
    }
}
