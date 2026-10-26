// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! Flow control on UDP 4455 (Gen 3 phase B, plan Task 4): a receiver asks
//! the transmitter for a flow of channels, the transmitter answers with a
//! handle and sends the audio to the address the request names; the
//! receiver keeps it alive with a datagram on the flow at least every 4 s,
//! and stops or changes it with its handle.
//!
//! - [`control`]: the datagrams (inferno's `flows_control.rs` and the
//!   parse in its `flows_control_server.rs`).
//! - [`server`]: our answers to them, as inferno answers each one, and the
//!   server on UDP 4455 that feeds the TX flow table
//!   ([`crate::media::tx::TxTable`]).
//!
//! - [`client`]: our own requests to a transmitter (the RX side, plan Task
//!   5): a flow of its channels to one of our flow sockets, and its stop.
//!
//! The TX flows themselves (the table, the schedule and the thread that
//! sends) are [`crate::media::tx`]; our RX flows are
//! [`crate::subscribe::Manager`]'s (when to ask) and [`crate::media::rx`]'s
//! (the thread that receives).

pub mod client;
pub mod control;
pub mod server;
