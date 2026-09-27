//! The guard pipe's messages (S6 design note §5.1).
//!
//! `iemmode`, the tray and HIL jobs talk to the guard in the engine's framing:
//! a little-endian `u32` length and that many bytes of JSON, here at most
//! [`MAX_FRAME`]. Every mutation of the PC goes through this pipe.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::alarms::Alarm;
use crate::plan::Mode;
use crate::state::Switching;

/// The guard pipe's name; the single-instance mutex is `Global\` + this.
pub const NAME: &str = "iemmixer-guard";

/// Largest frame body in bytes.
pub const MAX_FRAME: usize = 64 * 1024;

/// The `detail` of the update that asks a subscribed tray to quit (the
/// guard's `tray_stop`).
pub const TRAY_QUIT: &str = "quit";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Event {
        dry_run: bool,
    },
    Dev {
        build: Option<String>,
        force: bool,
        dry_run: bool,
    },
    Live {
        build: String,
        trial: bool,
        dry_run: bool,
    },
    Install {
        zip: String,
    },
    Activate {
        sha: String,
    },
    /// Card-masked to `[guard] hil_tx` by the guard (design §4); a request
    /// never chooses the card outputs.
    TestSignal {
        input: String,
        dbfs: f64,
        ttl_s: f64,
    },
    Report {
        sha: String,
        hil: String,
        detail: String,
    },
    /// Refused unless dev, not switching, band activity quiet for 5 min and a
    /// 60 s stage-input peak check from the engine's meters is quiet.
    JobBegin {
        run: u64,
    },
    JobEnd {
        run: u64,
    },
    InstallSite {
        path: String,
    },
    ForceReopen,
    /// Dev only: stop the idle runner (bootstrap check, S6 plan Task 16).
    RunnerStop,
    /// Starts `\iemmixer\iemmixer-probe` from the guard (design §5.1); any
    /// mode, it runs `cmd /c exit 0` only.
    ProbeTask,
    /// Dev only: the teardown half of the event plan without REAPER, then
    /// back into dev (never starts REAPER, so it is not a switch).
    RehearseTeardown,
    AlarmTest,
    AlarmAck {
        id: u64,
    },
    Quit,
    Subscribe,
}

/// Every answer, and every update a subscriber gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    pub mode: Mode,
    /// The switch in progress, if any.
    pub switching: Option<Switching>,
    /// The kept alarms, newest last (spec §4.2: shown on every call).
    #[serde(default)]
    pub alarms: Vec<Alarm>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("guard pipe i/o: {0}")]
    Io(#[from] io::Error),
    /// The announced or produced body exceeds [`MAX_FRAME`].
    #[error("frame of {0} bytes exceeds the 64 KiB limit")]
    TooLarge(usize),
    /// The peer closed the stream between frames.
    #[error("guard pipe closed")]
    Closed,
    /// The body is not the expected JSON message.
    #[error("bad guard message: {0}")]
    Bad(String),
}

/// Writes one frame (length and body in one write).
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let body = serde_json::to_vec(msg).map_err(|e| FrameError::Bad(e.to_string()))?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|_| body.len() <= MAX_FRAME)
        .ok_or(FrameError::TooLarge(body.len()))?;
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()?;
    Ok(())
}

/// Reads one frame's body. A clean end of stream before the first byte is
/// [`FrameError::Closed`]; one inside the frame is `UnexpectedEof`; a larger
/// announced frame is refused before its body is read.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, FrameError> {
    let mut head = [0u8; 4];
    let (first, rest) = head.split_at_mut(1);
    loop {
        match r.read(first) {
            Ok(0) => return Err(FrameError::Closed),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    r.read_exact(rest)?;
    let len = u32::from_le_bytes(head) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Parses a frame body as `T`.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    serde_json::from_slice(body).map_err(|e| FrameError::Bad(e.to_string()))
}

/// Reads one frame and parses it as `T`.
pub fn read_msg<T: DeserializeOwned, R: Read>(r: &mut R) -> Result<T, FrameError> {
    decode(&read_frame(r)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarms::Alarms;
    use crate::plan::Step;

    fn every_request() -> Vec<Request> {
        vec![
            Request::Status,
            Request::Event { dry_run: true },
            Request::Dev {
                build: Some("0123456789abcdef0123456789abcdef01234567".into()),
                force: false,
                dry_run: true,
            },
            Request::Dev {
                build: None,
                force: true,
                dry_run: false,
            },
            Request::Live {
                build: "0123456789abcdef0123456789abcdef01234567".into(),
                trial: true,
                dry_run: false,
            },
            Request::Install {
                zip: "C:\\bundles\\iemmixer.zip".into(),
            },
            Request::Activate {
                sha: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            Request::TestSignal {
                input: "mic1".into(),
                dbfs: -24.5,
                ttl_s: 30.0,
            },
            Request::Report {
                sha: "0123456789abcdef0123456789abcdef01234567".into(),
                hil: "green".into(),
                detail: "120 s at 32, 0 missed".into(),
            },
            Request::JobBegin { run: 4242 },
            Request::JobEnd { run: 4242 },
            Request::InstallSite {
                path: "site.toml".into(),
            },
            Request::ForceReopen,
            Request::RunnerStop,
            Request::ProbeTask,
            Request::RehearseTeardown,
            Request::AlarmTest,
            Request::AlarmAck { id: 7 },
            Request::Quit,
            Request::Subscribe,
        ]
    }

    #[test]
    fn every_request_round_trips_through_a_frame() {
        let all = every_request();
        let mut wire = Vec::new();
        for req in &all {
            write_frame(&mut wire, req).unwrap();
        }
        let mut r = wire.as_slice();
        for req in &all {
            assert_eq!(&read_msg::<Request, _>(&mut r).unwrap(), req);
        }
        assert!(matches!(read_frame(&mut r), Err(FrameError::Closed)));
    }

    #[test]
    fn requests_are_tagged_by_cmd_in_snake_case() {
        let json = |r: &Request| serde_json::to_string(r).unwrap();
        assert_eq!(json(&Request::Status), r#"{"cmd":"status"}"#);
        assert_eq!(
            json(&Request::Event { dry_run: false }),
            r#"{"cmd":"event","dry_run":false}"#
        );
        assert_eq!(
            json(&Request::JobBegin { run: 1 }),
            r#"{"cmd":"job_begin","run":1}"#
        );
        assert_eq!(
            json(&Request::RehearseTeardown),
            r#"{"cmd":"rehearse_teardown"}"#
        );
        assert_eq!(
            decode::<Request>(br#"{"cmd":"alarm_ack","id":3}"#).unwrap(),
            Request::AlarmAck { id: 3 }
        );
    }

    #[test]
    fn replies_round_trip_with_alarms_and_a_switch() {
        let mut alarms = Alarms::default();
        alarms.raise(
            1_790_000_000,
            Some(Step::PrefCheck),
            "3 restores failed",
            true,
        );
        let reply = Reply {
            ok: false,
            mode: Mode::Dev,
            switching: Some(Switching {
                from: Mode::Dev,
                to: Mode::Event,
                done: vec![Step::EngineStop],
                started: 1_790_000_000,
            }),
            alarms: alarms.all().to_vec(),
            detail: "switching".into(),
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &reply).unwrap();
        assert_eq!(read_msg::<Reply, _>(&mut wire.as_slice()).unwrap(), reply);
        // Older or newer peers: missing lists and texts default.
        assert_eq!(
            decode::<Reply>(br#"{"ok":true,"mode":"event","switching":null}"#).unwrap(),
            Reply {
                ok: true,
                mode: Mode::Event,
                switching: None,
                alarms: Vec::new(),
                detail: String::new(),
            }
        );
    }

    #[test]
    fn the_length_prefix_is_little_endian() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Request::Quit).unwrap();
        let body = br#"{"cmd":"quit"}"#;
        assert_eq!(&wire[..4], &(body.len() as u32).to_le_bytes());
        assert_eq!(&wire[4..], body);
    }

    #[test]
    fn garbage_is_refused() {
        for body in [
            &b"not json"[..],
            &br#"{"cmd":"reboot"}"#[..],
            &br#"{"cmd":"event"}"#[..],
            &b""[..],
        ] {
            let mut wire = (body.len() as u32).to_le_bytes().to_vec();
            wire.extend_from_slice(body);
            match read_msg::<Request, _>(&mut wire.as_slice()) {
                Err(FrameError::Bad(_)) => {}
                other => panic!("{body:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn oversize_is_refused_before_the_body() {
        let mut wire = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(b"{}");
        match read_frame(&mut wire.as_slice()) {
            Err(FrameError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("{other:?}"),
        }
        let mut huge = u32::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(matches!(
            read_frame(&mut huge.as_slice()),
            Err(FrameError::TooLarge(_))
        ));
        // A body of exactly MAX_FRAME bytes is allowed.
        let mut exact = (MAX_FRAME as u32).to_le_bytes().to_vec();
        exact.resize(4 + MAX_FRAME, b' ');
        assert_eq!(read_frame(&mut exact.as_slice()).unwrap().len(), MAX_FRAME);
    }

    #[test]
    fn an_oversize_message_is_not_written() {
        // `"x…x"`: the quotes make the body two bytes longer than the text.
        let fits = "x".repeat(MAX_FRAME - 2);
        let mut wire = Vec::new();
        write_frame(&mut wire, &fits).unwrap();
        assert_eq!(wire.len(), 4 + MAX_FRAME);
        let over = "x".repeat(MAX_FRAME - 1);
        let mut none = Vec::new();
        match write_frame(&mut none, &over) {
            Err(FrameError::TooLarge(n)) => assert_eq!(n, MAX_FRAME + 1),
            other => panic!("{other:?}"),
        }
        assert!(none.is_empty());
    }

    #[test]
    fn truncation_is_an_unexpected_eof() {
        for wire in [&[5u8, 0][..], &[5, 0, 0, 0, b'{'][..]] {
            match read_frame(&mut &wire[..]) {
                Err(FrameError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
                other => panic!("{wire:?}: {other:?}"),
            }
        }
    }

    /// A reader that fails once with `kind`, then serves `data`.
    struct FailOnce<'a> {
        kind: Option<io::ErrorKind>,
        data: &'a [u8],
    }

    impl Read for FailOnce<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if let Some(kind) = self.kind.take() {
                return Err(kind.into());
            }
            self.data.read(out)
        }
    }

    #[test]
    fn an_interruption_is_retried_and_other_errors_end_the_read() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Request::Status).unwrap();
        let mut interrupted = FailOnce {
            kind: Some(io::ErrorKind::Interrupted),
            data: &wire,
        };
        assert_eq!(
            read_msg::<Request, _>(&mut interrupted).unwrap(),
            Request::Status
        );
        let mut broken = FailOnce {
            kind: Some(io::ErrorKind::ConnectionReset),
            data: &wire,
        };
        match read_frame(&mut broken) {
            Err(FrameError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn errors_display() {
        assert_eq!(FrameError::Closed.to_string(), "guard pipe closed");
        assert_eq!(
            FrameError::TooLarge(7).to_string(),
            "frame of 7 bytes exceeds the 64 KiB limit"
        );
        assert_eq!(
            FrameError::Bad("x".into()).to_string(),
            "bad guard message: x"
        );
        let io: FrameError = io::Error::other("boom").into();
        assert_eq!(io.to_string(), "guard pipe i/o: boom");
        assert_eq!(NAME, "iemmixer-guard");
        assert_eq!(TRAY_QUIT, "quit");
    }
}
