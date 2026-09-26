//! The mix state the engine owns (program spec §2.4, I6) and its transient
//! state (solo, listen, test signal: never persisted). Values are in dB, Hz,
//! octaves and pan −1…1; the engine caps every field on arrival.
//!
//! Every type defaults its missing fields and ignores unknown ones, so a newer
//! or older writer's file still loads (additive schemas).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{BusId, InputId, SendId, Source};

/// Persisted schema version; it only ever goes up, and only additively.
pub const SCHEMA: u32 = 1;

/// Gains at or below this many dB are silence (linear 0).
pub const DB_OFF: f64 = -150.0;

/// Linear gain of `db`; at or below [`DB_OFF`] (and for NaN) exactly 0.
pub fn db_to_lin(db: f64) -> f64 {
    if db > DB_OFF {
        10f64.powf(db / 20.0)
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    HighPass,
    LowShelf,
    #[default]
    Peak,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub kind: BandKind,
    pub enabled: bool,
    pub freq_hz: f64,
    /// [`DB_OFF`] or below is gain 0: a notch on a peak band (A12).
    pub gain_db: f64,
    pub bw_oct: f64,
}

impl Default for EqBand {
    fn default() -> Self {
        Self {
            kind: BandKind::Peak,
            enabled: false,
            freq_hz: 1000.0,
            gain_db: 0.0,
            bw_oct: 1.0,
        }
    }
}

/// A five-band EQ (A12) with its global gain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Eq {
    pub gain_db: f64,
    pub bands: [EqBand; 5],
}

const fn off(kind: BandKind, freq_hz: f64, bw_oct: f64) -> EqBand {
    EqBand {
        kind,
        enabled: false,
        freq_hz,
        gain_db: 0.0,
        bw_oct,
    }
}

impl Default for Eq {
    /// ReaEQ's standard flat layout (`iem_dsp::eq::EqParams::standard_flat`).
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            bands: [
                off(BandKind::HighPass, 80.20834168547682, 2.0),
                off(BandKind::LowShelf, 200.3077623397404, 2.0),
                off(BandKind::Peak, 801.9398157380639, 1.0),
                off(BandKind::Peak, 2996.2342070275295, 1.0),
                off(BandKind::HighShelf, 8016.061124722856, 2.0),
            ],
        }
    }
}

/// A bus limiter (A13, F12): limit −6…0 dB.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limiter {
    pub enabled: bool,
    pub limit_db: f64,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            enabled: true,
            limit_db: -6.0,
        }
    }
}

/// An input strip: trim and EQ run only with `processing` (A4, Q3); the fader
/// and pan feed the master (A11); mute silences every tap (A3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputState {
    pub trim_db: f64,
    pub muted: bool,
    pub processing: bool,
    pub fader_db: f64,
    pub pan: f64,
    pub eq: Eq,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            trim_db: 0.0,
            muted: false,
            processing: true,
            fader_db: 0.0,
            pan: 0.0,
            eq: Eq::default(),
        }
    }
}

/// A bus: EQ and limiter apply where the topology gives the bus one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BusState {
    pub fader_db: f64,
    pub pan: f64,
    pub muted: bool,
    pub eq: Eq,
    pub limiter: Limiter,
}

impl Default for BusState {
    fn default() -> Self {
        Self {
            fader_db: 0.0,
            pan: 0.0,
            muted: false,
            eq: Eq::default(),
            limiter: Limiter::default(),
        }
    }
}

/// A send (A5): off by default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SendState {
    pub gain_db: f64,
    pub pan: f64,
    pub muted: bool,
}

impl Default for SendState {
    fn default() -> Self {
        Self {
            gain_db: DB_OFF,
            pan: 0.0,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendEntry {
    pub id: SendId,
    #[serde(default)]
    pub state: SendState,
}

/// Everything the engine persists about the mix (§2.4). Sends are sorted by id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixState {
    pub inputs: BTreeMap<InputId, InputState>,
    pub buses: BTreeMap<BusId, BusState>,
    pub sends: Vec<SendEntry>,
}

/// A solo mask on one bus's tree (X2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Solo {
    pub scope: BusId,
    pub sources: Vec<Source>,
}

/// A running test signal (X13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestSignal {
    pub input: InputId,
    pub hz: f64,
    pub dbfs: f64,
    pub ttl_s: f64,
}

/// Engine state that is never persisted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transient {
    pub solo: Vec<Solo>,
    /// Slot 0: the engineer's listen tap; slot 1: one member's (X3).
    pub listen: [Option<BusId>; 2],
    pub test_signal: Option<TestSignal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_to_lin_floors_at_db_off() {
        assert_eq!(db_to_lin(DB_OFF), 0.0);
        assert_eq!(db_to_lin(-1e9), 0.0);
        assert_eq!(db_to_lin(f64::NAN), 0.0);
        assert_eq!(db_to_lin(0.0), 1.0);
        assert_eq!(db_to_lin(-6.0), 10f64.powf(-0.3));
        assert!(db_to_lin(DB_OFF + 1e-9) > 0.0);
    }

    #[test]
    fn eq_default_is_the_reaeq_standard_flat() {
        let dsp = iem_dsp::eq::EqParams::standard_flat();
        let ours = Eq::default();
        assert_eq!(ours.gain_db, 0.0);
        assert_eq!(dsp.global_gain, 1.0);
        for (a, b) in ours.bands.iter().zip(dsp.bands.iter()) {
            let kind = match b.kind {
                iem_dsp::eq::BandKind::HighPass => BandKind::HighPass,
                iem_dsp::eq::BandKind::LowShelf => BandKind::LowShelf,
                iem_dsp::eq::BandKind::Peak => BandKind::Peak,
                iem_dsp::eq::BandKind::HighShelf => BandKind::HighShelf,
            };
            assert_eq!(a.kind, kind);
            assert_eq!(a.freq_hz, b.freq_hz);
            assert_eq!(a.bw_oct, b.bw_oct);
            assert_eq!(db_to_lin(a.gain_db), b.gain_lin);
            assert_eq!(a.enabled, b.enabled);
        }
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let i = InputState::default();
        assert_eq!(
            (i.trim_db, i.muted, i.processing, i.fader_db, i.pan),
            (0.0, false, true, 0.0, 0.0)
        );
        let b = BusState::default();
        assert_eq!((b.fader_db, b.pan, b.muted), (0.0, 0.0, false));
        assert_eq!(
            b.limiter,
            Limiter {
                enabled: true,
                limit_db: -6.0
            }
        );
        let s = SendState::default();
        assert_eq!((s.gain_db, s.pan, s.muted), (DB_OFF, 0.0, false));
        let band = EqBand::default();
        assert_eq!(
            (
                band.kind,
                band.enabled,
                band.freq_hz,
                band.gain_db,
                band.bw_oct
            ),
            (BandKind::Peak, false, 1000.0, 0.0, 1.0)
        );
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        let s: MixState = serde_json::from_str(
            r#"{"inputs":{"mic1":{"trim_db":-3,"future":1}},"buses":{"member1":{"muted":true,"eq":{"bands":[{},{},{},{},{"kind":"high_shelf"}]}}},"sends":[{"id":{"src":{"input":"mic1"},"dst":"member1"}}],"later":[1,2]}"#,
        )
        .unwrap();
        let input = &s.inputs[&InputId::new("mic1")];
        assert_eq!(input.trim_db, -3.0);
        assert!(input.processing);
        assert_eq!(input.eq, Eq::default());
        let bus = &s.buses[&BusId::new("member1")];
        assert!(bus.muted);
        assert_eq!(bus.eq.bands[0], EqBand::default());
        assert_eq!(bus.eq.bands[4].kind, BandKind::HighShelf);
        assert_eq!(s.sends[0].state, SendState::default());
        let t: Transient = serde_json::from_str("{}").unwrap();
        assert_eq!(t, Transient::default());
    }

    #[test]
    fn state_round_trips_through_json() {
        let mut s = MixState::default();
        s.inputs.insert(
            InputId::new("mic1"),
            InputState {
                trim_db: 1.25,
                pan: -0.3,
                ..InputState::default()
            },
        );
        s.buses.insert(BusId::new("member1"), BusState::default());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<MixState>(&json).unwrap(), s);
        assert!(json.contains(r#""kind":"high_pass""#));
    }
}
