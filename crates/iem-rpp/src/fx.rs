//! FX chain blocks: ReaEQ, the trim JSFX and the output limiter JSFX, the
//! plug-in allowlist and deterministic GUIDs.

use sha2::{Digest, Sha256};

use crate::reaeq::{HEAD as REAEQ_HEAD, ReaEq};
use crate::rpp::{Chunk, RppError, num};

pub const TRIM_HEAD: &str = r#"JS utility/volume_pan """#;
pub const LIMITER_HEAD: &str = r#"JS loser/MGA_JSLimiterST """#;
/// Every plug-in head a generated project may contain (P5). The bundle
/// check and the PC stager repeat this list.
pub const ALLOWED_FX_HEADS: [&str; 3] = [REAEQ_HEAD, TRIM_HEAD, LIMITER_HEAD];

#[derive(Debug, Clone, PartialEq)]
pub enum Fx {
    ReaEq(ReaEq),
    /// `utility/volume_pan` slider 1 in dB.
    Trim {
        db: f64,
    },
    /// `loser/MGA_JSLimiterST` at threshold = ceiling = `limit_db`, 50 ms, 75 %.
    Limiter {
        limit_db: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxSlot {
    pub fx: Fx,
    pub bypassed: bool,
}

impl FxSlot {
    pub const fn active(fx: Fx) -> Self {
        Self {
            fx,
            bypassed: false,
        }
    }
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` from the seed's sha256.
pub fn guid(seed: &str) -> String {
    let hex: String = Sha256::digest(seed.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02X}"))
        .collect();
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn js(head: &str, sliders: &[f64]) -> Result<Chunk, RppError> {
    let mut fields = sliders
        .iter()
        .map(|v| num(*v))
        .collect::<Result<Vec<_>, _>>()?;
    fields.resize(64, "-".to_owned());
    let mut c = Chunk::new(head);
    c.line(fields.join(" "));
    Ok(c)
}

pub fn fx_chunk(fx: &Fx) -> Result<Chunk, RppError> {
    match fx {
        Fx::ReaEq(eq) => eq.chunk(),
        Fx::Trim { db } => js(TRIM_HEAD, &[*db, 0.0, 0.0]),
        Fx::Limiter { limit_db } => js(LIMITER_HEAD, &[*limit_db, 50.0, 75.0, *limit_db, 0.0]),
    }
}

pub fn fx_chain(slots: &[FxSlot], seed: &str) -> Result<Chunk, RppError> {
    let mut c = Chunk::new("FXCHAIN");
    c.line("SHOW 0").line("LASTSEL 0").line("DOCKED 0");
    for (i, slot) in slots.iter().enumerate() {
        c.line(format!("BYPASS {} 0 0", u8::from(slot.bypassed)));
        c.child(fx_chunk(&slot.fx)?);
        c.line("FLOATPOS 0 0 0 0");
        c.line(format!("FXID {}", guid(&format!("{seed}/fx{i}"))));
        c.line("WAK 0 0");
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_are_deterministic_and_well_formed() {
        let g = guid("a");
        assert_eq!(g, guid("a"));
        assert_ne!(g, guid("b"));
        assert_eq!(g.len(), 38);
        assert_eq!(g.matches('-').count(), 4);
    }

    #[test]
    fn js_blocks_carry_64_slider_fields() {
        let c = fx_chunk(&Fx::Limiter { limit_db: -6.0 }).unwrap().render();
        assert!(c.starts_with("<JS loser/MGA_JSLimiterST \"\"\n  -6 50 75 -6 0 - "));
        let fields = c.lines().nth(1).unwrap().split_whitespace().count();
        assert_eq!(fields, 64);
        assert!(
            fx_chunk(&Fx::Trim { db: 6.0 })
                .unwrap()
                .render()
                .contains("\n  6 0 0 - ")
        );
    }

    #[test]
    fn chain_marks_bypass_per_slot() {
        let slots = [
            FxSlot::active(Fx::Trim { db: 6.0 }),
            FxSlot {
                fx: Fx::Trim { db: 1.0 },
                bypassed: true,
            },
        ];
        let text = fx_chain(&slots, "t").unwrap().render();
        let bypass: Vec<&str> = text
            .lines()
            .filter(|l| l.trim_start().starts_with("BYPASS"))
            .collect();
        assert_eq!(bypass, vec!["  BYPASS 0 0 0", "  BYPASS 1 0 0"]);
        for head in ALLOWED_FX_HEADS {
            assert!(!head.starts_with('<'));
        }
    }
}
