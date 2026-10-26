// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The audio path (Gen 3 phase B): the packet codec, the jitter ring of an
//! RX channel, the TX test sources and the checkers of what comes back
//! (plan Task 3, pure), and the TX flows with their thread ([`tx`], plan
//! Task 4).
//!
//! The hot path: [`packet`]'s write and read, [`ring::Ring`]'s write and
//! read, every [`source::Source`] and both checkers in [`check`] allocate
//! nothing, take no lock, log nothing and make no system call
//! (`tests/hot_path.rs` runs each under `assert_no_alloc`). Allocation
//! happens once, when a ring or a checker is built.
//!
//! A sample is a left-justified `i32` (a 24-bit sample `<< 8`, a 16-bit
//! one `<< 16`), as inferno keeps them: full scale is 2³¹ whatever the
//! flow's encoding, so phase C scales by 2⁻³¹.

pub mod check;
pub mod packet;
pub mod ring;
pub mod source;
pub mod tx;
