// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/mdns_client.rs (`query_chan`: the channel's record
// `<channel>@<device>._netaudio-chan._udp.local.`, its address and port, the
// TXT keys it reads (`id`, `nchan`, `enc` or `en`, `dbcp1`, `fpp` as
// "most,least", `latency_ns`) and their numbers, decimal or `0x` hex),
// Copyright (C) Teo and the inferno contributors, GPL-3.0-or-later OR
// AGPL-3.0-or-later, taken under GPL-3.0-or-later. Changes Copyright (C)
// 2026 iemmixer contributors.

//! Where a subscribed channel comes from: the transmitter's mDNS record of
//! that channel (`_netaudio-chan`), read into an [`Advertised`]: the
//! transmitter's flow-control address, the channel's id and the flow
//! formats it offers. Inferno reads these keys; we also read `rate`, so a
//! transmitter at another sample rate is never asked (it would refuse,
//! 0x0301).
//!
//! - [`Resolver`]: the manager's view, a lookup by device and channel name
//!   (case aside, as mDNS compares names); no lookup sends anything.
//! - [`Adverts`]: the records, kept from the events of one mDNS browse,
//!   bounded at [`MAX_ADVERTS`].
//! - [`Mdns`]: the records of the device's own mdns-sd daemon (on our
//!   interface only, as the daemon is set up): **one** browse of
//!   `_netaudio-chan` for the device's life, started at the first
//!   subscription ([`Mdns::start`], at most once), its events kept by a
//!   thread of their own. The queries it sends are mdns-sd's: a PTR query
//!   1 s after the start, then at doubling intervals up to one an hour,
//!   and each cached record's refresh at 80–95 % of its TTL (RFC 6762
//!   §5.2); a transmitter's announcement (a reboot) reaches it with no
//!   query of ours. Nothing here queries per lookup or per retry.
//! - [`Own`]: our own device's channels, from the records it announces
//!   (a subscription to our own TX is resolved without the network).
//!
//! A device no channel record names ([`Adverts::names_device`]) is read
//! through its ARC instead ([`super::directory`], plan Task 9b: the band's
//! Yamaha Dante cards publish no `_netaudio-chan` record); [`Browse`] starts
//! that directory's browses too.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};

use crate::announce::{CHAN_SERVICE, records};
use crate::daemon::host_of;
use crate::device::{DeviceConfig, DeviceState};

/// The most channel records kept (a band's network has hundreds; a flood
/// of invented ones stops here).
pub const MAX_ADVERTS: usize = 4096;

/// A transmitter's channel as its record offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advertised {
    /// The transmitter's flow control (UDP 4455): where a request goes.
    pub control: SocketAddrV4,
    /// The channel's id in a flow request (from 1).
    pub tx_channel_id: u16,
    /// The most channel slots of one flow from it.
    pub nchan: u16,
    /// Bits per sample: 16, 24 or 32.
    pub bits: u32,
    pub rate: u32,
    /// Its flow control's start code (read, not used: inferno's client
    /// always sends 0x1102).
    pub dbcp1: u16,
    /// The least and most frames per packet it sends.
    pub fpp_min: u16,
    pub fpp_max: u16,
    /// The least latency a receiver may set, ns.
    pub latency_ns: u64,
}

/// Why a record was not read: the key, never its value or the record's
/// name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdvertError {
    #[error("the channel record has no {0}")]
    Missing(&'static str),
    #[error("the channel record's {0} is not valid")]
    Invalid(&'static str),
    #[error("the channel record has no IPv4 address")]
    NoAddress,
}

/// Reads a channel record's TXT (`get` gives a key's value) for the
/// transmitter whose flow control is `control`.
pub fn parse<'a>(
    get: impl Fn(&str) -> Option<&'a str>,
    control: SocketAddrV4,
) -> Result<Advertised, AdvertError> {
    let number = |key: &'static str| -> Result<u64, AdvertError> {
        let value = get(key).ok_or(AdvertError::Missing(key))?;
        int(value).ok_or(AdvertError::Invalid(key))
    };
    let within = |key: &'static str, low: u64, high: u64| -> Result<u64, AdvertError> {
        Some(number(key)?)
            .filter(|v| (low..=high).contains(v))
            .ok_or(AdvertError::Invalid(key))
    };
    let u16_max = u64::from(u16::MAX);
    let bits_key = if get("enc").is_some() { "enc" } else { "en" };
    let bits = number(bits_key).map_err(|e| match e {
        AdvertError::Missing(_) => AdvertError::Missing("enc"),
        other => other,
    })?;
    if ![16, 24, 32].contains(&bits) {
        return Err(AdvertError::Invalid(bits_key));
    }
    let fpp = get("fpp").ok_or(AdvertError::Missing("fpp"))?;
    let (most, least) = fpp
        .split_once(',')
        .and_then(|(m, l)| Some((int(m)?, int(l)?)))
        .filter(|&(m, l)| (1..=u16_max).contains(&m) && l <= u16_max)
        .ok_or(AdvertError::Invalid("fpp"))?;
    Ok(Advertised {
        control,
        tx_channel_id: within("id", 1, u16_max)? as u16,
        nchan: within("nchan", 1, u16_max)? as u16,
        bits: bits as u32,
        rate: within("rate", 1, u64::from(u32::MAX))? as u32,
        dbcp1: within("dbcp1", 0, u16_max)? as u16,
        fpp_min: least as u16,
        fpp_max: most as u16,
        latency_ns: number("latency_ns")?,
    })
}

/// A TXT number: decimal, or hex after `0x` (inferno's
/// `parse_int_from_dict`).
fn int(s: &str) -> Option<u64> {
    match s.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// A resolved record's lowest IPv4 address (one interface's, as our daemon
/// listens on one).
pub fn lowest_ipv4(r: &ResolvedService) -> Option<Ipv4Addr> {
    r.get_addresses()
        .iter()
        .filter_map(|a| match a.to_ip_addr() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .min()
}

/// A resolved record's name (case aside) and channel: its lowest IPv4
/// address ([`lowest_ipv4`]) and its port.
pub fn from_resolved(r: &ResolvedService) -> Result<(String, Advertised), AdvertError> {
    let ip = lowest_ipv4(r).ok_or(AdvertError::NoAddress)?;
    let advertised = parse(
        |key| r.get_property_val_str(key),
        SocketAddrV4::new(ip, r.get_port()),
    )?;
    Ok((r.get_fullname().to_ascii_lowercase(), advertised))
}

/// The record name of `channel` on `device`, as the records are kept (case
/// aside).
pub fn instance(device: &str, channel: &str) -> String {
    format!("{channel}@{device}.{CHAN_SERVICE}").to_ascii_lowercase()
}

/// The manager's lookups.
pub trait Resolver {
    /// The record of `tx_channel` on `tx_device`, if one is known now.
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised>;
}

/// Our own records first, then the second resolver's.
impl<A: Resolver, B: Resolver> Resolver for (A, B) {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0
            .lookup(tx_device, tx_channel)
            .or_else(|| self.1.lookup(tx_device, tx_channel))
    }
}

/// What one browse event did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// A record kept (new or newer).
    Resolved,
    /// A record gone.
    Removed,
    /// Nothing changed: another service, a record that does not read, a new
    /// record past [`MAX_ADVERTS`], or an event of the search itself.
    Ignored,
}

/// The channel records known, by name (case aside), and how many each
/// device has.
#[derive(Debug, Default)]
pub struct Adverts {
    map: HashMap<String, Advertised>,
    devices: HashMap<String, usize>,
}

/// The device a channel record's name (as [`instance`] writes it) names.
fn device_of(name: &str) -> Option<&str> {
    let (_, device) = name
        .strip_suffix(CHAN_SERVICE)?
        .strip_suffix('.')?
        .rsplit_once('@')?;
    Some(device)
}

impl Adverts {
    /// Keeps what a browse event says.
    pub fn apply(&mut self, ev: &ServiceEvent) -> Applied {
        match ev {
            ServiceEvent::ServiceResolved(r) if r.ty_domain == CHAN_SERVICE => {
                let Ok((name, a)) = from_resolved(r) else {
                    return Applied::Ignored;
                };
                if self.map.len() >= MAX_ADVERTS && !self.map.contains_key(&name) {
                    return Applied::Ignored;
                }
                if let Some(device) = device_of(&name).filter(|_| !self.map.contains_key(&name)) {
                    *self.devices.entry(device.to_string()).or_default() += 1;
                }
                self.map.insert(name, a);
                Applied::Resolved
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                let name = fullname.to_ascii_lowercase();
                if self.map.remove(&name).is_none() {
                    return Applied::Ignored;
                }
                if let Some(device) = device_of(&name)
                    && let Some(n) = self.devices.get_mut(device)
                {
                    *n -= 1;
                    if *n == 0 {
                        self.devices.remove(device);
                    }
                }
                Applied::Removed
            }
            _ => Applied::Ignored,
        }
    }

    /// The record of `channel` on `device`.
    pub fn get(&self, device: &str, channel: &str) -> Option<Advertised> {
        self.map.get(&instance(device, channel)).copied()
    }

    /// Whether any channel record names `device` (case aside): such a
    /// device resolves through its records only, never through its ARC
    /// ([`crate::subscribe::directory::Network`]).
    pub fn names_device(&self, device: &str) -> bool {
        self.devices.contains_key(&device.to_ascii_lowercase())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Resolver for Adverts {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.get(tx_device, tx_channel)
    }
}

/// Our own device's channels, read from the records it announces at `ip`
/// (built again after a rename).
#[derive(Debug, Default)]
pub struct Own(Adverts);

impl Own {
    pub fn new(state: &DeviceState, cfg: &DeviceConfig, ip: Ipv4Addr) -> Self {
        let mut adverts = Adverts::default();
        for rec in records(state, cfg, &host_of(&state.name), ip)
            .iter()
            .filter(|r| r.service == CHAN_SERVICE)
        {
            if let Ok(info) = rec.info() {
                adverts.apply(&ServiceEvent::ServiceResolved(Box::new(
                    info.as_resolved_service(),
                )));
            }
        }
        Own(adverts)
    }
}

impl Resolver for Own {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.0.get(tx_device, tx_channel)
    }
}

/// Where a browse's events come from: the mDNS daemon, or a test's
/// channel.
pub trait Browse {
    type Events: Iterator<Item = ServiceEvent> + Send + 'static;
    /// Starts browsing `service` (`_netaudio-chan`, and for the ARC reads
    /// `_netaudio-arc` and `_netaudio-dbc`); the events end when the daemon
    /// does.
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error>;
}

impl Browse for ServiceDaemon {
    type Events = <mdns_sd::Receiver<ServiceEvent> as IntoIterator>::IntoIter;
    fn browse(&self, service: &str) -> Result<Self::Events, mdns_sd::Error> {
        Ok(ServiceDaemon::browse(self, service)?.into_iter())
    }
}

/// Why the browse did not start.
#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error("mDNS browse: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error("the browse's thread: {0}")]
    Thread(#[from] io::Error),
}

/// The channel records our mDNS daemon's one browse finds.
#[derive(Debug, Default)]
pub struct Mdns {
    adverts: Arc<Mutex<Adverts>>,
    thread: Option<JoinHandle<()>>,
}

impl Mdns {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the browse and the thread keeping its records, once for this
    /// resolver's life: `true` when it started now, `false` when it had.
    pub fn start<B: Browse>(&mut self, daemon: &B) -> Result<bool, BrowseError> {
        if self.thread.is_some() {
            return Ok(false);
        }
        let events = daemon.browse(CHAN_SERVICE)?;
        let adverts = Arc::clone(&self.adverts);
        let thread = std::thread::Builder::new()
            .name("dante-mdns-chan".to_string())
            .spawn(move || keep(events, &adverts))?;
        self.thread = Some(thread);
        Ok(true)
    }

    pub fn started(&self) -> bool {
        self.thread.is_some()
    }

    /// Whether any channel record names `device` ([`Adverts::names_device`]).
    pub fn names_device(&self, device: &str) -> bool {
        self.adverts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .names_device(device)
    }

    /// Waits for the thread to end (the daemon's events ended), at most
    /// `limit`.
    #[cfg(test)]
    fn join_within(&mut self, limit: std::time::Duration) {
        let thread = self.thread.take().expect("started");
        let deadline = std::time::Instant::now() + limit;
        while !thread.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the thread never ended"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        thread.join().unwrap();
    }
}

impl Resolver for Mdns {
    fn lookup(&self, tx_device: &str, tx_channel: &str) -> Option<Advertised> {
        self.adverts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(tx_device, tx_channel)
    }
}

/// The browse thread: every event into `adverts`, until the events end.
fn keep(events: impl Iterator<Item = ServiceEvent>, adverts: &Mutex<Adverts>) {
    for ev in events {
        adverts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply(&ev);
    }
}

#[cfg(test)]
mod tests;
