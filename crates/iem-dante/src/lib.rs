// iem-dante: iemmixer's own Dante device (Gen 3, #72).
// Copyright (C) 2026 iemmixer contributors.
// Parts adapted from inferno (https://github.com/teodly/inferno at 9767558),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken here under GPL-3.0-or-later; each adapted file
// names its origin in its header.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! iemmixer's own Dante device (Gen 3 design §5.3, phase A): a box that
//! Dante Controller lists under its own name, with its RX and TX channels,
//! and whose channel renames stick across restarts. Phase B (plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-b-audio-through.md`) adds
//! the media clock and the audio path, piece by piece; the device never
//! sends PTP.
//!
//! - [`device`]: the config (a private TOML on the box) and the persisted
//!   names, with Dante's name rules.
//! - [`wire`]: the request/response datagram ARC and CMC share.
//! - [`arc`]: the control server on UDP 4440: names, channel lists,
//!   renames, the empty flow lists; every opcode inferno answers is
//!   answered as inferno answers it, the rest get no reply and are counted
//!   in one warning a minute.
//! - [`cmc`]: the advertisement on UDP 8800.
//! - [`info`]: the info requests on UDP 8700 (replies to the info group
//!   224.0.0.231:8702) and the heartbeat every second to 224.0.0.233:8708;
//!   Dante Controller goes on to ARC only once its board info request is
//!   answered.
//! - [`announce`]: the mDNS records Dante Controller browses for, and their
//!   announcer.
//! - [`clock`]: the media clock: PTP time as `CLOCK_MONOTONIC` + k, k from
//!   dantesync's time server on the box (the box's one PTP owner), and the
//!   packets' stamps.
//! - [`daemon`]: the `iem-dante-dev` binary's command line, its interface
//!   and address, and the loop that serves the device and announces it
//!   again after renames.
//!
//! The device answers for itself only: it never sends a command that
//! changes another Dante device.

pub mod announce;
pub mod arc;
pub mod clock;
pub mod cmc;
pub mod daemon;
pub mod device;
pub mod info;
pub mod wire;
