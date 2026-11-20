// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The mic-to-ear latency guard in the box's health (#122): the paths
//! `scripts/iem-pc/gen3_latency.py` computes for the gates, judged here from
//! the numbers deploy.sh installs as [`GUARD`] (the admin user who runs
//! health reads neither the site nor iempc's env) and the device's status
//! line.
//!
//! An input's path = D (`engine.offset`) + the worst `delay` of the `rx[]`
//! flows whose `rx_channels` hold one of its RX channels (its worst channel),
//! plus the in-ear receivers' latency (the worst `receiver_latency_us` of the
//! `tx[]` flows whose `tx_channels` carry an in-ear output, in samples
//! rounded up), against its budget. A channel no flow carries adds nothing
//! and an input none of whose channels a flow carries is not known: a stage
//! device switched off never makes health unhealthy (deploy waits for
//! healthy); the gates fail closed on it. [`Judgment::reasons`]:
//! [`Reason::LatencyOverBudget`] for a known path above its budget,
//! [`Reason::LatencyUnread`] for an in-ear TX flow whose receiver latency
//! is unknown (dstack's 0), a guard file that does not read, or a status
//! line that does not read as flows. Pure; [`read_guard`] reads the file.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Reason;

/// Where deploy.sh installs the guard's numbers (0644, root's).
pub const GUARD: &str = "/etc/iemmixer/latency-guard.json";

/// The guard's numbers (`gen3_latency.guard`): the device's rate, the
/// in-ear TX channels and every guarded input's RX channels with its
/// budget; no id or name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    pub rate: u64,
    pub in_ear_tx: Vec<u64>,
    pub inputs: Vec<Input>,
}

/// One guarded input: its RX channels (from 1) and its budget in samples.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub rx: Vec<u64>,
    pub budget: u64,
}

/// The guard file as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardRead {
    /// No file: a box before the deploy that installs it; nothing judged.
    Absent,
    /// A file that cannot be read or is not the guard's shape.
    Unreadable,
    Read(Guard),
}

/// The guard's text: `None` unless it is the guard's shape with a rate
/// above 0 and every channel from 1, each input on at least one.
pub fn parse_guard(text: &str) -> Option<Guard> {
    let guard: Guard = serde_json::from_str(text).ok()?;
    let channel = |n: &u64| *n >= 1;
    let valid = guard.rate > 0
        && guard.in_ear_tx.iter().all(channel)
        && guard
            .inputs
            .iter()
            .all(|i| !i.rx.is_empty() && i.rx.iter().all(channel));
    valid.then_some(guard)
}

/// The guard file at `path`: absent when it does not exist, unreadable for
/// any other error or a text [`parse_guard`] refuses.
pub fn read_guard(path: &Path) -> GuardRead {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_guard(&text).map_or(GuardRead::Unreadable, GuardRead::Read),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GuardRead::Absent,
        Err(_) => GuardRead::Unreadable,
    }
}

/// What the guard reads of a status line (dstack v0.6.137): the engine's D
/// and the flows; every other key passed over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Flows {
    #[serde(default)]
    pub engine: Option<Engine>,
    #[serde(default)]
    pub rx: Vec<RxFlow>,
    #[serde(default)]
    pub tx: Vec<TxFlow>,
}

/// The status's `engine`, its D.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Engine {
    #[serde(default)]
    pub offset: Option<u64>,
}

/// An `rx[]` row: the RX channels it feeds (absent for a slot without a
/// flow now) and its delay in samples.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RxFlow {
    #[serde(default)]
    pub rx_channels: Option<Vec<u64>>,
    #[serde(default)]
    pub delay: Option<u64>,
}

/// A `tx[]` row: its slot count, the TX channel of each slot (`null` for
/// an empty one; absent without a flow) and its receiver's latency, whole
/// µs (0: not known).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TxFlow {
    #[serde(default)]
    pub channels: u64,
    #[serde(default)]
    pub tx_channels: Option<Vec<Option<u64>>>,
    #[serde(default)]
    pub receiver_latency_us: u64,
}

/// A status line's flows; `None` for a line that does not read as them.
pub fn parse_flows(line: &str) -> Option<Flows> {
    serde_json::from_str(line).ok()
}

/// What the line shows of the guard: numbers or null only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Line {
    /// The guarded inputs.
    pub inputs: u64,
    /// Those whose path is known.
    pub known: u64,
    /// Those above their budget.
    pub over: u64,
    /// The in-ear TX flows whose receiver latency is not known.
    pub unread_tx: u64,
    /// The longest known path, samples.
    pub worst_samples: Option<u64>,
    /// 1 ms at the device's rate, samples (rounded down).
    pub budget_samples: Option<u64>,
}

/// The guard's judgment: the line and its reasons, each at most once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgment {
    pub line: Line,
    pub reasons: Vec<Reason>,
}

/// `us` microseconds in whole samples at `rate`, rounded up.
fn samples_of_us(us: u64, rate: u64) -> u64 {
    us.saturating_mul(rate).div_ceil(1_000_000)
}

/// Whether a TX flow with slots carries an in-ear output.
fn in_ear(flow: &TxFlow, outputs: &[u64]) -> bool {
    flow.channels > 0
        && flow
            .tx_channels
            .iter()
            .flatten()
            .flatten()
            .any(|c| outputs.contains(c))
}

/// The worst delay of the RX flows feeding our RX channel `rx`.
fn flow_delay(flows: &Flows, rx: u64) -> Option<u64> {
    flows
        .rx
        .iter()
        .filter(|f| f.rx_channels.as_ref().is_some_and(|c| c.contains(&rx)))
        .filter_map(|f| f.delay)
        .max()
}

/// The guard against one status read's flows (`None`: the line did not
/// read as flows).
pub fn judge(guard: &Guard, flows: Option<&Flows>) -> Judgment {
    let mut line = Line {
        inputs: guard.inputs.len() as u64,
        budget_samples: Some(guard.rate / 1_000),
        ..Line::default()
    };
    let Some(flows) = flows else {
        return Judgment {
            line,
            reasons: vec![Reason::LatencyUnread],
        };
    };
    let carrying: Vec<&TxFlow> = flows
        .tx
        .iter()
        .filter(|f| in_ear(f, &guard.in_ear_tx))
        .collect();
    line.unread_tx = carrying
        .iter()
        .filter(|f| f.receiver_latency_us == 0)
        .count() as u64;
    let ear = carrying
        .iter()
        .map(|f| f.receiver_latency_us)
        .filter(|&us| us > 0)
        .max()
        .map(|us| samples_of_us(us, guard.rate));
    let d = flows.engine.as_ref().and_then(|e| e.offset);
    if let (Some(d), Some(ear)) = (d, ear) {
        for input in &guard.inputs {
            let Some(delay) = input
                .rx
                .iter()
                .filter_map(|&rx| flow_delay(flows, rx))
                .max()
            else {
                continue;
            };
            let samples = d.saturating_add(delay).saturating_add(ear);
            line.known += 1;
            line.over += u64::from(samples > input.budget);
            line.worst_samples = line.worst_samples.max(Some(samples));
        }
    }
    let mut reasons = Vec::new();
    if line.over > 0 {
        reasons.push(Reason::LatencyOverBudget);
    }
    if line.unread_tx > 0 {
        reasons.push(Reason::LatencyUnread);
    }
    Judgment { line, reasons }
}

/// The guard as the box has it, against the shown read's status line
/// (`None` without one): the line's `latency` (`None` without a file) and
/// the reasons. An unreadable file shows zeros and no budget.
pub fn assess(guard: &GuardRead, status_line: Option<&str>) -> (Option<Line>, Vec<Reason>) {
    match guard {
        GuardRead::Absent => (None, Vec::new()),
        GuardRead::Unreadable => (Some(Line::default()), vec![Reason::LatencyUnread]),
        GuardRead::Read(guard) => match status_line {
            // No read at all: health names it (`no_status`), nothing more here.
            None => (Some(judge(guard, Some(&Flows::default())).line), Vec::new()),
            Some(text) => {
                let j = judge(guard, parse_flows(text).as_ref());
                (Some(j.line), j.reasons)
            }
        },
    }
}

#[cfg(test)]
mod tests;
