//! Protocol values → DSP parameters, and the caps the engine applies to every
//! field from any sender (§2.3: no pipe tokens, so the engine trusts nothing).
//! Shared by the control core (commands) and the processor (initial state).

use iem_dsp::eq::{Band, BandKind as DspKind, EqParams};
use iem_engine_proto::{
    BandKind, DB_OFF, Eq, EqBand, InputState, Level, MixGroup, MixOut, db_to_lin,
};

/// Inclusive ranges.
pub type Range = (f64, f64);
pub const TRIM_DB: Range = (DB_OFF, 24.0);
/// F5/F7: levels, group faders and mix volumes up to +12 dB; at or below
/// `DB_OFF` silent.
pub const FADER_DB: Range = (DB_OFF, 12.0);
pub const PAN: Range = (-1.0, 1.0);
/// ReaEQ's range (A12).
pub const EQ_FREQ: Range = (20.0, 24_000.0);
/// Up to ReaEQ's +12.04 dB (linear 4).
pub const EQ_GAIN_DB: Range = (DB_OFF, 12.041199826559248);
pub const EQ_BW: Range = (0.01, 4.0);
/// F12.
pub const LIMIT_DB: Range = (-6.0, 0.0);
/// X13 test signal.
pub const TEST_HZ: Range = (20.0, 20_000.0);
pub const TEST_DBFS: Range = (-120.0, -20.0);
pub const TEST_TTL_S: Range = (0.001, 120.0);

/// `v` clamped into `r`; `None` when `v` is not finite.
pub fn cap(v: f64, r: Range) -> Option<f64> {
    v.is_finite().then(|| v.clamp(r.0, r.1))
}

fn fix(v: f64, r: Range, default: f64) -> f64 {
    cap(v, r).unwrap_or(default)
}

/// Every EQ value finite (commands reject the rest; loads replace it).
pub fn eq_is_finite(e: &Eq) -> bool {
    e.gain_db.is_finite()
        && e.bands
            .iter()
            .all(|b| b.freq_hz.is_finite() && b.gain_db.is_finite() && b.bw_oct.is_finite())
}

pub fn cap_eq(e: &Eq) -> Eq {
    let d = EqBand::default();
    Eq {
        gain_db: fix(e.gain_db, EQ_GAIN_DB, 0.0),
        bands: e.bands.map(|b| EqBand {
            freq_hz: fix(b.freq_hz, EQ_FREQ, d.freq_hz),
            gain_db: fix(b.gain_db, EQ_GAIN_DB, d.gain_db),
            bw_oct: fix(b.bw_oct, EQ_BW, d.bw_oct),
            ..b
        }),
    }
}

pub fn cap_input(s: &InputState) -> InputState {
    InputState {
        trim_db: fix(s.trim_db, TRIM_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    }
}

pub fn cap_level(s: &Level) -> Level {
    Level {
        gain_db: fix(s.gain_db, FADER_DB, DB_OFF),
        pan: fix(s.pan, PAN, 0.0),
        ..*s
    }
}

pub fn cap_group(s: &MixGroup) -> MixGroup {
    MixGroup {
        gain_db: fix(s.gain_db, FADER_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    }
}

pub fn cap_out(s: &MixOut) -> MixOut {
    let mut o = MixOut {
        volume_db: fix(s.volume_db, FADER_DB, 0.0),
        eq: cap_eq(&s.eq),
        ..*s
    };
    o.limiter.limit_db = fix(s.limiter.limit_db, LIMIT_DB, LIMIT_DB.0);
    o
}

fn dsp_kind(k: BandKind) -> DspKind {
    match k {
        BandKind::HighPass => DspKind::HighPass,
        BandKind::LowShelf => DspKind::LowShelf,
        BandKind::Peak => DspKind::Peak,
        BandKind::HighShelf => DspKind::HighShelf,
    }
}

/// The DSP form of a (capped) EQ.
pub fn eq_params(e: &Eq) -> EqParams {
    EqParams {
        bands: e.bands.map(|b| Band {
            kind: dsp_kind(b.kind),
            enabled: b.enabled,
            freq_hz: b.freq_hz,
            gain_lin: db_to_lin(b.gain_db),
            bw_oct: b.bw_oct,
        }),
        global_gain: db_to_lin(e.gain_db),
    }
}

/// An input strip's linear parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputParams {
    pub trim: f64,
    pub muted: bool,
    pub processing: bool,
}

pub fn input_params(s: &InputState) -> InputParams {
    InputParams {
        trim: db_to_lin(s.trim_db),
        muted: s.muted,
        processing: s.processing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_clamps_and_rejects_non_finite() {
        assert_eq!(cap(40.0, FADER_DB), Some(12.0));
        assert_eq!(cap(-1e308, FADER_DB), Some(DB_OFF));
        assert_eq!(cap(3.0, PAN), Some(1.0));
        assert_eq!(cap(-0.25, PAN), Some(-0.25));
        assert_eq!(cap(f64::NAN, PAN), None);
        assert_eq!(cap(f64::INFINITY, PAN), None);
        assert_eq!(EQ_GAIN_DB.1, 20.0 * 4f64.log10());
    }

    #[test]
    fn state_caps_replace_non_finite_with_defaults() {
        let mut eq = Eq {
            gain_db: f64::NAN,
            ..Eq::default()
        };
        eq.bands[1].freq_hz = 1e9;
        eq.bands[2].gain_db = f64::NEG_INFINITY;
        eq.bands[3].bw_oct = 0.0;
        let c = cap_eq(&eq);
        assert_eq!(c.gain_db, 0.0);
        assert_eq!(c.bands[1].freq_hz, 24_000.0);
        assert_eq!(c.bands[2].gain_db, 0.0);
        assert_eq!(c.bands[3].bw_oct, 0.01);
        assert_eq!(c.bands[0], eq.bands[0]);
        assert!(!eq_is_finite(&eq));
        assert!(eq_is_finite(&c));
        let cases: [fn(&mut Eq); 3] = [
            |e| e.bands[0].freq_hz = f64::NAN,
            |e| e.bands[4].bw_oct = f64::INFINITY,
            |e| e.gain_db = f64::INFINITY,
        ];
        for bad in cases {
            let mut e = Eq::default();
            bad(&mut e);
            assert!(!eq_is_finite(&e));
        }
        let i = cap_input(&InputState {
            trim_db: 99.0,
            muted: true,
            processing: false,
            eq,
        });
        assert_eq!((i.trim_db, i.muted, i.processing), (24.0, true, false));
        assert_eq!(i.eq, c);
        assert_eq!(
            cap_input(&InputState {
                trim_db: f64::NAN,
                ..InputState::default()
            })
            .trim_db,
            0.0
        );
        let l = cap_level(&Level {
            gain_db: f64::NAN,
            pan: 2.0,
            muted: true,
        });
        assert_eq!((l.gain_db, l.pan, l.muted), (DB_OFF, 1.0, true));
        let l = cap_level(&Level {
            gain_db: 40.0,
            pan: f64::NAN,
            muted: false,
        });
        assert_eq!((l.gain_db, l.pan), (12.0, 0.0));
        assert_eq!(
            cap_level(&Level {
                gain_db: -3.0,
                ..Level::default()
            })
            .gain_db,
            -3.0
        );
        let g = cap_group(&MixGroup {
            gain_db: f64::INFINITY,
            muted: true,
            eq,
        });
        assert_eq!((g.gain_db, g.muted), (0.0, true));
        assert_eq!(g.eq, c);
        assert_eq!(
            cap_group(&MixGroup {
                gain_db: -1e308,
                ..MixGroup::default()
            })
            .gain_db,
            DB_OFF
        );
        let mut o = MixOut {
            volume_db: 13.0,
            muted: true,
            eq,
            ..MixOut::default()
        };
        o.limiter.limit_db = -9.0;
        let co = cap_out(&o);
        assert_eq!(
            (co.volume_db, co.muted, co.limiter.limit_db),
            (12.0, true, -6.0)
        );
        assert_eq!(co.eq, c);
        o.volume_db = f64::NAN;
        o.limiter.limit_db = f64::NAN;
        assert_eq!(
            (cap_out(&o).volume_db, cap_out(&o).limiter.limit_db),
            (0.0, -6.0)
        );
        o.limiter.limit_db = 3.0;
        o.limiter.enabled = false;
        assert_eq!(cap_out(&o).limiter.limit_db, 0.0);
        assert!(!cap_out(&o).limiter.enabled);
    }

    #[test]
    fn eq_params_convert_to_linear() {
        let mut eq = Eq {
            gain_db: -6.0,
            ..Eq::default()
        };
        eq.bands[2].enabled = true;
        eq.bands[2].gain_db = 6.0;
        eq.bands[3].gain_db = DB_OFF;
        let p = eq_params(&eq);
        assert_eq!(p.global_gain, 10f64.powf(-0.3));
        assert_eq!(p.bands[2].gain_lin, 10f64.powf(0.3));
        assert!(p.bands[2].enabled);
        assert_eq!(p.bands[3].gain_lin, 0.0);
        let flat = iem_dsp::eq::EqParams::standard_flat();
        assert_eq!(eq_params(&Eq::default()), flat);
        let kinds = [
            BandKind::HighPass,
            BandKind::LowShelf,
            BandKind::Peak,
            BandKind::HighShelf,
        ];
        let dsp = [
            DspKind::HighPass,
            DspKind::LowShelf,
            DspKind::Peak,
            DspKind::HighShelf,
        ];
        for (k, d) in kinds.into_iter().zip(dsp) {
            assert_eq!(dsp_kind(k), d);
        }
    }

    #[test]
    fn input_params_are_linear() {
        let p = input_params(&InputState {
            trim_db: -6.0,
            muted: true,
            processing: false,
            eq: Eq::default(),
        });
        assert_eq!(p.trim, 10f64.powf(-0.3));
        assert_eq!((p.muted, p.processing), (true, false));
        assert_eq!(input_params(&InputState::default()).trim, 1.0);
    }
}
