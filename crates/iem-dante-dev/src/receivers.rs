// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev receivers` (#72, the receiver side's gate item): what
//! Dante Controller's latency monitor shows for the receivers of our TX,
//! read on the box. Every Dante receiver says it in its heartbeat, once a
//! second, to the heartbeat group (dstack's [`iem_dante::latency`]): record
//! 0x8003 each RX flow slot's largest lateness that second, 0x8004 each
//! slot's late packets since the device started. Dante Controller bins the
//! peaks against each flow's latency (the receiver's ARC 0x3200 RX flow
//! list); red is a packet past it.
//!
//! The read, all of it read-only: the devices on our address's network
//! (dstack's [`iem_dante::discover`], its mDNS sweep), our own name over
//! our own ARC (0x1002: a name Dante Controller set counts), then for every
//! other device with a heartbeat id its RX channels (0x1000, 0x3000) and,
//! when one subscribes to our name, its RX flows (0x3200): the slots that
//! feed such a channel are our flows there ([`our_slots`]). Then the
//! heartbeats for `--seconds`, gathered per device id, and [`report`].
//! Nothing is ever written to another device.
//!
//! One JSON line ([`Report::line`]), numbers only: no device name, address
//! or id; a receiver is an opaque `key` (a hash of its heartbeat id, stable
//! within one build) so a gate can follow a slot from read to read. Exit 0
//! with the line (red or not: the line's `red`), 1 when the read could not
//! run (no socket, our own ARC silent), 2 for the command line.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::Ipv4Addr;

use iem_dante::arc::control::{RxChannel, RxFlow};
use iem_dante::latency::Latency;
use serde::Serialize;

/// The heartbeats are listened to this long by default, seconds.
pub const SECONDS: u32 = 5;
/// The longest listen, seconds.
pub const MAX_SECONDS: u32 = 60;

/// What `receivers`' command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Our device's address on the Dante network.
    pub ip: Ipv4Addr,
    /// The heartbeats' listen, seconds (1 to [`MAX_SECONDS`]).
    pub seconds: u32,
}

/// The command line (without the program's name): `None` when its first
/// word is not `receivers`, else [`parse`] of the rest.
pub fn parse_command(args: &[String]) -> Option<Result<Option<Options>, String>> {
    match args.split_first() {
        Some((first, rest)) if first == "receivers" => Some(parse(rest)),
        _ => None,
    }
}

/// `receivers`' arguments: `--ip <IPv4>` (required) and `--seconds <n>`,
/// each at most once; `Ok(None)` for `--help` or `-h`.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut ip = None;
    let mut seconds = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--ip" => &mut ip,
            "--seconds" => &mut seconds,
            other => return Err(format!("receivers: unknown argument {other:?}")),
        };
        let Some(value) = it.next() else {
            return Err(format!("{arg} needs a value"));
        };
        if slot.replace(value).is_some() {
            return Err(format!("{arg} given twice"));
        }
    }
    let Some(ip) = ip else {
        return Err("receivers: --ip <our Dante IPv4 address> is required".into());
    };
    let ip: Ipv4Addr = ip
        .parse()
        .map_err(|_| "--ip is not an IPv4 address".to_string())?;
    let seconds = match seconds {
        None => SECONDS,
        Some(text) => match text.parse::<u32>() {
            Ok(n) if (1..=MAX_SECONDS).contains(&n) => n,
            _ => {
                return Err(format!(
                    "--seconds is not a whole number from 1 to {MAX_SECONDS}"
                ));
            }
        },
    };
    Ok(Some(Options { ip, seconds }))
}

/// `receivers`' usage, for the binary called `name`.
pub fn usage(name: &str) -> String {
    format!(
        "usage: {name} receivers --ip <our Dante IPv4> [--seconds <n>]

receivers: what the receivers of our TX say about our packets, as Dante
Controller's latency monitor shows it: their heartbeats for --seconds
(default {SECONDS}, at most {MAX_SECONDS}), each flow of ours there with its
latency, its largest lateness, its late packets in the listen and in all.
Read-only: mDNS, ARC reads and listening; nothing is written to any device.
One JSON line of numbers (no name, address or id); exit 0 with the line,
1 when the read could not run, 2 for the command line."
    )
}

/// One flow of ours at a receiver: its RX flow slot there (1 up, the
/// heartbeat's index + 1) and the latency it plays it at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ours {
    pub slot: u16,
    pub latency_ns: u32,
}

/// The receiver's flows that carry our TX: each slot of `flows` feeding an
/// RX channel of `channels` subscribed to the device `ours` (Dante's
/// names, case aside), in slot order.
pub fn our_slots(ours: &str, channels: &[RxChannel], flows: &[RxFlow]) -> Vec<Ours> {
    let fed: BTreeSet<u16> = channels
        .iter()
        .filter(|c| {
            c.tx_device
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(ours))
        })
        .map(|c| c.channel)
        .collect();
    let mut out: Vec<Ours> = flows
        .iter()
        .filter(|f| f.channels.iter().any(|c| fed.contains(c)))
        .map(|f| Ours {
            slot: f.slot,
            latency_ns: f.latency_ns,
        })
        .collect();
    out.sort_by_key(|o| o.slot);
    out.dedup_by_key(|o| o.slot);
    out
}

/// A receiver whose heartbeat is listened to: its heartbeat id and our
/// flows there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub device_id: [u8; 8],
    pub flows: Vec<Ours>,
}

/// A receiver's opaque key: a hash of its heartbeat id, 16 hex digits
/// (the same within one build; never the id itself).
pub fn key_of(device_id: &[u8; 8]) -> String {
    let mut h = DefaultHasher::new();
    device_id.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// One flow of ours at one receiver, as the line shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowReport {
    pub key: String,
    pub slot: u16,
    pub latency_us: u32,
    /// The receiver's heartbeats heard in the listen.
    pub beats: u32,
    /// The largest lateness of a second in the listen, µs (`None`: no
    /// lateness record heard for the slot).
    pub peak_us: Option<u32>,
    /// Late packets in the listen.
    pub late: u64,
    /// Late packets since the receiver started, at its last heartbeat.
    pub late_total: u32,
}

/// What a read found (see the module).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Devices the sweep listed (ours included).
    pub devices: usize,
    /// Devices whose ARC reads failed.
    pub unread: usize,
    /// Devices listed without a heartbeat id (not read).
    pub no_id: usize,
    /// Receivers of our TX.
    pub receivers: usize,
    /// Of them, those whose heartbeat was heard.
    pub heard: usize,
    pub flows: Vec<FlowReport>,
    /// A flow with a late packet in the listen or a peak past its latency.
    pub red: bool,
}

impl Report {
    /// The one JSON line: `{"receivers": {...}}`.
    pub fn line(&self) -> String {
        #[derive(Serialize)]
        struct Line<'a> {
            receivers: &'a Report,
        }
        // Numbers, booleans and hex keys only: nothing here can fail.
        serde_json::to_string(&Line { receivers: self }).expect("the receivers line serializes")
    }
}

/// The report of a read: `devices`, `unread` and `no_id` counted by the
/// read, `targets` the receivers of our TX, `heard` what the heartbeats
/// gave (any devices; the targets' are taken by id).
pub fn report(
    devices: usize,
    unread: usize,
    no_id: usize,
    targets: &[Target],
    heard: &[Latency],
) -> Report {
    let mut flows = Vec::new();
    let mut heard_n = 0;
    for t in targets {
        let got = heard.iter().find(|l| l.device_id == t.device_id);
        if got.is_some_and(|l| l.beats > 0) {
            heard_n += 1;
        }
        let key = key_of(&t.device_id);
        for o in &t.flows {
            let slot = got.and_then(|l| l.slots.iter().find(|s| s.slot == o.slot));
            flows.push(FlowReport {
                key: key.clone(),
                slot: o.slot,
                latency_us: o.latency_ns / 1_000,
                beats: got.map_or(0, |l| l.beats),
                peak_us: slot.and_then(|s| s.peaks_us.iter().copied().max()),
                late: slot.map_or(0, |s| s.late),
                late_total: slot.map_or(0, |s| s.late_total),
            });
        }
    }
    let red = flows
        .iter()
        .any(|f| f.late > 0 || f.peak_us.is_some_and(|p| p > f.latency_us));
    Report {
        devices,
        unread,
        no_id,
        receivers: targets.len(),
        heard: heard_n,
        flows,
        red,
    }
}

#[cfg(target_os = "linux")]
pub use glue::read;

/// The Linux glue: discovery, the ARC reads and the heartbeats' listen.
#[cfg(target_os = "linux")]
mod glue {
    use std::net::SocketAddrV4;
    use std::sync::atomic::{AtomicBool, AtomicU16};
    use std::time::{Duration, Instant};

    use iem_dante::arc::client::{REPLY_TIMEOUT, ReadError, Udp};
    use iem_dante::arc::control::{device_name, rx_channels, rx_flows};
    use iem_dante::ctl::Requests;
    use iem_dante::discover::Discovery;
    use iem_dante::latency::{Heard, Listener};

    use super::{Options, Report, Target, our_slots, report};

    /// How long discovery lists the devices before they are read.
    pub const DISCOVER: Duration = Duration::from_secs(3);

    /// One read (see the module); `Err` with a fixed text when it cannot
    /// run.
    pub fn read(opts: &Options) -> Result<Report, String> {
        let seq = AtomicU16::new(1);
        let requests = Requests::default();
        let stop = AtomicBool::new(false);
        let arc = |to: SocketAddrV4| Udp::open(opts.ip, to, &seq, REPLY_TIMEOUT, &requests, &stop);
        let ours = arc(SocketAddrV4::new(opts.ip, iem_dante::arc::PORT))
            .map_err(|e| format!("no socket on our address: {}", e.kind()))
            .and_then(|mut u| {
                device_name(&mut u).map_err(|e| format!("our own ARC: {}", e.label()))
            })?;
        let (discovery, _changes) =
            Discovery::start(opts.ip).map_err(|e| format!("discovery did not start: {e}"))?;
        std::thread::sleep(DISCOVER);
        let devices = discovery.devices();
        // Its threads end within its own bound; a slow stop changes no read.
        let _ = discovery.stop();
        let (mut unread, mut no_id) = (0, 0);
        let mut targets = Vec::new();
        for d in devices
            .iter()
            .filter(|d| !d.name.eq_ignore_ascii_case(&ours))
        {
            let Some(device_id) = d.device_id else {
                no_id += 1;
                continue;
            };
            let flows = arc(d.arc)
                .map_err(|e| ReadError::Io {
                    op: 0,
                    kind: e.kind(),
                })
                .and_then(|mut u| {
                    let channels = rx_channels(&mut u)?;
                    if channels.iter().any(|c| {
                        c.tx_device
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&ours))
                    }) {
                        Ok(our_slots(&ours, &channels, &rx_flows(&mut u)?))
                    } else {
                        Ok(Vec::new())
                    }
                });
            match flows {
                Ok(flows) if !flows.is_empty() => targets.push(Target { device_id, flows }),
                Ok(_) => {}
                Err(_) => unread += 1,
            }
        }
        let (listener, beats) =
            Listener::start(opts.ip).map_err(|e| format!("no heartbeat socket: {}", e.kind()))?;
        let mut heard = Heard::default();
        let end = Instant::now() + Duration::from_secs(u64::from(opts.seconds));
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match beats.recv_timeout(left) {
                Ok((_, beat)) if targets.iter().any(|t| t.device_id == beat.device_id) => {
                    heard.add(&beat);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        drop(listener);
        Ok(report(
            devices.len(),
            unread,
            no_id,
            &targets,
            &heard.result(),
        ))
    }
}

#[cfg(test)]
mod tests;
