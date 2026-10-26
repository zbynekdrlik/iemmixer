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
//! and whose channel renames stick across restarts. No audio, no clock and
//! no flows yet (phases B and C); the device never sends PTP.
//!
//! - [`device`]: the config (a private TOML on the box) and the persisted
//!   names, with Dante's name rules.
//! - [`wire`]: the request/response datagram ARC and CMC share.
//!
//! The device answers for itself only: it never sends a command that
//! changes another Dante device.

pub mod device;
pub mod wire;
