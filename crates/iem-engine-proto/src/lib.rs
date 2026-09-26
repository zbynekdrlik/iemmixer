//! The engine ↔ server protocol (S3; program spec §2.3, §2.4, I6; design note
//! `docs/superpowers/specs/2026-09-26-s3-engine-core-design.md` §3.3, §3.6):
//!
//! - [`ids`]: stable string ids of inputs, groups and mixes (#20 design note);
//! - [`state`]: the persisted mix state and the transient engine state;
//! - [`msg`]: commands, replies and events, protocol N/N−1 negotiation;
//! - [`frame`]: the control pipe's length-prefixed JSON frames;
//! - [`media`]: the media pipe's binary 48 kHz frames.
//!
//! Schemas are additive (§2.4): readers ignore unknown fields and default
//! missing ones, so no type here denies unknown fields.

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

pub mod frame;
pub mod ids;
pub mod media;
pub mod msg;
pub mod state;

pub use frame::{FrameError, MAX_FRAME, read_frame, write_frame};
pub use ids::{EqTarget, GroupId, InputId, MAX_ID_LEN, MixId, Source, valid_id};
pub use media::{FRAME_48K, MEDIA_HEADER, MediaHeader, read_media, write_media};
pub use msg::{
    Alarm, AlarmCode, Change, ClientMsg, Cmd, EngineMsg, ErrCode, ErrorBody, GroupInfo, Hello,
    InputInfo, Meters, MixInfo, PROTO, Reply, Role, Status, TopologyInfo, negotiate, parse_client,
};
pub use state::{
    BandKind, DB_OFF, Eq, EqBand, InputState, Level, Limiter, Mix, MixGroup, MixOut, MixState,
    SCHEMA, Solo, TestSignal, Transient, db_to_lin,
};
