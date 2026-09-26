//! The mix state the engine owns (program spec §2.4, I6; #20 design note §3,
//! §5) and its transient state (solo, listen, test signal: never persisted).
//! Values are in dB, Hz, octaves and pan −1…1; the engine caps every field on
//! arrival.
//!
//! Every type defaults its missing fields and ignores unknown ones, so a newer
//! or older writer's file still loads (additive schemas).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, InputId, MixId, Source};

/// Persisted schema version; it only ever goes up. 2: the purpose-built model
/// (#20); files of schema 1 (the REAPER-shaped graph) are refused on load.
pub const SCHEMA: u32 = 2;

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

/// A mix's limiter (A13, F12): limit −6…0 dB.
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

/// An input strip (A2–A4, F11, F29): trim and EQ run only with `processing`
/// (Q3); `muted` silences the input in every mix, talkback included (A3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputState {
    pub trim_db: f64,
    pub processing: bool,
    pub muted: bool,
    pub eq: Eq,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            trim_db: 0.0,
            processing: true,
            muted: false,
            eq: Eq::default(),
        }
    }
}

/// The level of one source in a mix (F5, F16; A5): off by default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Level {
    pub gain_db: f64,
    pub pan: f64,
    pub muted: bool,
}

impl Default for Level {
    fn default() -> Self {
        Self {
            gain_db: DB_OFF,
            pan: 0.0,
            muted: false,
        }
    }
}

/// A group's strip in one mix (F7, F11; A7): the group's inputs at their
/// levels, summed → EQ → fader → mute.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixGroup {
    pub gain_db: f64,
    pub muted: bool,
    pub eq: Eq,
}

impl Default for MixGroup {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            muted: false,
            eq: Eq::default(),
        }
    }
}

/// A mix's output (F7, F11, F12; A8): EQ → limiter → volume → mute.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixOut {
    pub volume_db: f64,
    pub muted: bool,
    pub eq: Eq,
    pub limiter: Limiter,
}

impl Default for MixOut {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            muted: false,
            eq: Eq::default(),
            limiter: Limiter::default(),
        }
    }
}

/// One listener's mix: its output, a level for every input, a strip for
/// every group and a level for each mix it hears.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mix {
    pub out: MixOut,
    pub inputs: BTreeMap<InputId, Level>,
    pub groups: BTreeMap<GroupId, MixGroup>,
    pub mixes: BTreeMap<MixId, Level>,
}

impl Mix {
    /// The level of `source`, if this mix holds one.
    pub fn level(&self, source: &Source) -> Option<&Level> {
        match source {
            Source::Input(id) => self.inputs.get(id),
            Source::Mix(id) => self.mixes.get(id),
        }
    }
}

/// Everything the engine persists about the mix (§2.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixState {
    pub inputs: BTreeMap<InputId, InputState>,
    pub mixes: BTreeMap<MixId, Mix>,
}

/// The soloed sources of one mix (F6, X2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Solo {
    pub mix: MixId,
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
    /// Slot 0: the engineer's listen tap; slot 1: one other mix's (X3).
    pub listen: [Option<MixId>; 2],
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
        assert_eq!((i.trim_db, i.processing, i.muted), (0.0, true, false));
        let l = Level::default();
        assert_eq!((l.gain_db, l.pan, l.muted), (DB_OFF, 0.0, false));
        let g = MixGroup::default();
        assert_eq!((g.gain_db, g.muted, g.eq), (0.0, false, Eq::default()));
        let o = MixOut::default();
        assert_eq!((o.volume_db, o.muted), (0.0, false));
        assert_eq!(
            o.limiter,
            Limiter {
                enabled: true,
                limit_db: -6.0
            }
        );
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
        let m = Mix::default();
        assert!(m.inputs.is_empty() && m.groups.is_empty() && m.mixes.is_empty());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        let s: MixState = serde_json::from_str(
            r#"{"inputs":{"mic1":{"trim_db":-3,"future":1}},"mixes":{"member1":{"out":{"muted":true,"eq":{"bands":[{},{},{},{},{"kind":"high_shelf"}]}},"inputs":{"mic1":{"pan":0.5}},"groups":{"stems":{"muted":true}},"mixes":{"member2":{}},"later":2}},"buses":[1,2]}"#,
        )
        .unwrap();
        let input = &s.inputs[&InputId::new("mic1")];
        assert_eq!(input.trim_db, -3.0);
        assert!(input.processing);
        assert_eq!(input.eq, Eq::default());
        let mix = &s.mixes[&MixId::new("member1")];
        assert!(mix.out.muted);
        assert_eq!(mix.out.volume_db, 0.0);
        assert_eq!(mix.out.eq.bands[0], EqBand::default());
        assert_eq!(mix.out.eq.bands[4].kind, BandKind::HighShelf);
        let mic1 = mix.inputs[&InputId::new("mic1")];
        assert_eq!((mic1.gain_db, mic1.pan), (DB_OFF, 0.5));
        assert!(mix.groups[&GroupId::new("stems")].muted);
        assert_eq!(mix.mixes[&MixId::new("member2")], Level::default());
        assert_eq!(
            mix.level(&Source::Input(InputId::new("mic1")))
                .map(|l| l.pan),
            Some(0.5)
        );
        assert_eq!(
            mix.level(&Source::Mix(MixId::new("member2"))),
            Some(&Level::default())
        );
        assert_eq!(mix.level(&Source::Mix(MixId::new("member3"))), None);
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
                ..InputState::default()
            },
        );
        let mut mix = Mix::default();
        mix.inputs.insert(
            InputId::new("mic1"),
            Level {
                gain_db: -3.0,
                pan: -0.3,
                muted: true,
            },
        );
        mix.groups
            .insert(GroupId::new("stems"), MixGroup::default());
        mix.mixes.insert(MixId::new("member2"), Level::default());
        s.mixes.insert(MixId::new("member1"), mix);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<MixState>(&json).unwrap(), s);
        assert!(json.contains(r#""kind":"high_pass""#));
        assert!(
            json.contains(r#""mixes":{"member1":{"out":{"volume_db":0.0"#),
            "{json}"
        );
    }
}
