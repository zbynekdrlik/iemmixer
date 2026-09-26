//! The iemmixer audio engine (S3; program spec §2, §3.3, §3.4; design note
//! `docs/superpowers/specs/2026-09-26-s3-engine-core-design.md`):
//!
//! - [`site`], [`graph`]: the `[engine]` table of `site.toml`, validated and
//!   compiled once per run (I4).
//!
//! GPL-3.0-or-later: the engine links the MGA limiter port (D1).

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod graph;
pub mod site;

/// The only rate the engine runs at (I2).
pub const SAMPLE_RATE: u32 = 96_000;

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use crate::graph::{Graph, compile};
    use crate::site::load;

    pub fn test_site_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
    }

    pub fn test_site_text() -> String {
        std::fs::read_to_string(test_site_path()).unwrap()
    }

    pub fn test_site() -> Graph {
        compile(&load(&test_site_path()).unwrap()).unwrap()
    }
}
