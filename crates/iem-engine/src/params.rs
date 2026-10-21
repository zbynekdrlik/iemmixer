//! Protocol values → DSP parameters, and the caps the engine applies to every
//! field from any sender (§2.3: no pipe tokens, so the engine trusts nothing).
//! Shared by the control core (commands) and the processor (initial state).

use iem_dsp::eq::{Band, BandKind as DspKind, EqParams};
use iem_engine_proto::{BandKind, Eq, InputState, db_to_lin};

/// The caps live with the protocol, so the server shapes a state as the
/// engine does (#58 re-review): `iem_engine_proto::shape`.
pub use iem_engine_proto::shape::{
    EQ_BW, EQ_FREQ, EQ_GAIN_DB, FADER_DB, LIMIT_DB, PAN, Range, TRIM_DB, cap, cap_eq, cap_group,
    cap_input, cap_level, cap_out, eq_is_finite,
};

/// X13 test signal.
pub const TEST_HZ: Range = (20.0, 20_000.0);
pub const TEST_DBFS: Range = (-120.0, -20.0);
pub const TEST_TTL_S: Range = (0.001, 120.0);

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
    use iem_engine_proto::DB_OFF;

    // The caps' own tests moved with them to `iem_engine_proto::shape`.

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
