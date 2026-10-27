// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from inferno (https://github.com/teodly/inferno at 9767558):
// inferno_aoip/src/device_server/mdns_server.rs (the services, their TXT
// entries, the channel records) and inferno_aoip/src/device_server/flows_tx.rs
// (the frames-per-packet range), Copyright (C) Teo and the inferno
// contributors, GPL-3.0-or-later OR AGPL-3.0-or-later, taken under
// GPL-3.0-or-later. Changes Copyright (C) 2026 iemmixer contributors.

//! The device's mDNS records, as Dante Controller browses for them:
//!
//! - `_netaudio-arc._udp` on 4440 and `_netaudio-cmc._udp` on 8800, both
//!   named by the device;
//! - one `_netaudio-chan._udp` per TX channel on the flow-control port
//!   4455, named `<channel>@<device>` by its default name and marked
//!   `default`, and one more by its current name while the two differ (a
//!   subscription made by either name still finds the channel).
//!
//! [`records`] is pure: the full set for a device. [`Announcer`] hands it to
//! the mDNS daemon, withdrawing the records that are gone before it
//! registers the new ones and sending nothing that did not change.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};

use mdns_sd::{ServiceDaemon, ServiceInfo, TxtProperty};

use crate::arc::{self, MAX_CHANNELS_IN_FLOW, PCM_TYPE};
use crate::cmc::{self, PROCESS_ID, device_id};
use crate::device::{DeviceConfig, DeviceState};

pub const ARC_SERVICE: &str = "_netaudio-arc._udp.local.";
pub const CMC_SERVICE: &str = "_netaudio-cmc._udp.local.";
pub const CHAN_SERVICE: &str = "_netaudio-chan._udp.local.";
/// Dante's flow-control port, where a receiver asks a transmitter for a
/// flow (inferno's `flows_control.rs`); the channel records point at it.
/// Phase A does not serve it.
pub const FLOWS_PORT: u16 = 4455;
/// The Dante model id inferno advertises (`model=`).
pub const MODEL_ID: &str = "_000000000000000b";
/// Frames per packet a channel offers, most and least (inferno's
/// `FPP_MAX_ADVERTISED` and `FPP_MIN`).
pub const FPP_MAX: u16 = 32;
pub const FPP_MIN: u16 = 2;

/// One mDNS service of the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRecord {
    /// The service type, e.g. [`CHAN_SERVICE`].
    pub service: &'static str,
    /// The instance name: the device's, or `<channel>@<device>`.
    pub instance: String,
    /// The host the SRV record points at, ending in `.local.`.
    pub host: String,
    /// The host's address (its A record).
    pub ip: Ipv4Addr,
    pub port: u16,
    /// The TXT strings in their order: `key=value`, a bare `key`, or empty.
    pub txt: Vec<String>,
}

impl ServiceRecord {
    /// The record as mdns-sd sends it. Pure: no socket is touched.
    pub fn info(&self) -> Result<ServiceInfo, mdns_sd::Error> {
        let properties: Vec<TxtProperty> = self
            .txt
            .iter()
            .map(|entry| match entry.split_once('=') {
                Some((key, value)) => TxtProperty::from((key, value)),
                None => TxtProperty::from(entry.as_str()),
            })
            .collect();
        ServiceInfo::new(
            self.service,
            &self.instance,
            &self.host,
            IpAddr::V4(self.ip),
            self.port,
            properties,
        )
    }
}

/// Every record of the device with `state`'s names, its SRV records
/// pointing at `host` (e.g. `<device>.local.`) at `ip`.
pub fn records(
    state: &DeviceState,
    cfg: &DeviceConfig,
    host: &str,
    ip: Ipv4Addr,
) -> Vec<ServiceRecord> {
    let record = |service, instance: String, port, txt: Vec<String>| ServiceRecord {
        service,
        instance,
        host: host.to_string(),
        ip,
        port,
        txt,
    };
    let mf = format!("mf={}", cfg.manufacturer);
    let model = format!("model={MODEL_ID}");
    let arc_txt = vec![
        "arcp_vers=2.7.41".to_string(),
        "arcp_min=0.2.4".to_string(),
        "router_vers=4.0.2".to_string(),
        format!("router_info={}", cfg.model),
        mf.clone(),
        model.clone(),
    ];
    let cmc_txt = vec![
        format!("id={}", hex(&device_id(ip))),
        format!("process={PROCESS_ID}"),
        "cmcp_vers=1.2.0".to_string(),
        "cmcp_min=1.0.0".to_string(),
        "server_vers=4.0.2".to_string(),
        "channels=0x6000004d".to_string(),
        mf,
        model,
        // inferno sends two empty strings last ("really needed?"); kept so
        // the record is byte for byte what Dante Controller accepts.
        String::new(),
        String::new(),
    ];
    let mut out = vec![
        record(ARC_SERVICE, state.name.clone(), arc::PORT, arc_txt),
        record(CMC_SERVICE, state.name.clone(), cmc::PORT, cmc_txt),
    ];
    let nchan = MAX_CHANNELS_IN_FLOW.min(u16::try_from(cfg.tx.len()).unwrap_or(u16::MAX));
    for (i, (default, now)) in cfg.tx.iter().zip(&state.tx).enumerate() {
        let txt = |is_default: bool| {
            let mut txt = vec![
                "txtvers=2".to_string(),
                "dbcp1=0x1102".to_string(),
                "dbcp=0x1004".to_string(),
                format!("id={}", i + 1),
                format!("rate={}", cfg.sample_rate),
                format!("pcm={} {PCM_TYPE:x}", cfg.encoding / 8),
                format!("enc={}", cfg.encoding),
                format!("en={}", cfg.encoding),
                format!("latency_ns={}", cfg.advertised_latency_ns()),
                format!("fpp={},{FPP_MIN}", cfg.tx_fpp_max),
                format!("nchan={nchan}"),
            ];
            if is_default {
                txt.push("default".to_string());
            }
            txt
        };
        let named = |channel: &str| format!("{channel}@{}", state.name);
        out.push(record(CHAN_SERVICE, named(default), FLOWS_PORT, txt(true)));
        // mDNS names compare without case: a name that differs only so is
        // the default record's own name.
        if !now.eq_ignore_ascii_case(default) {
            out.push(record(CHAN_SERVICE, named(now), FLOWS_PORT, txt(false)));
        }
    }
    out
}

/// Lower-case hex, two digits a byte.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where records go: the mDNS daemon, or a test's log of the calls.
pub trait Registry {
    fn register(&self, info: ServiceInfo) -> Result<(), mdns_sd::Error>;
    fn unregister(&self, fullname: &str) -> Result<(), mdns_sd::Error>;
}

impl Registry for ServiceDaemon {
    fn register(&self, info: ServiceInfo) -> Result<(), mdns_sd::Error> {
        ServiceDaemon::register(self, info)
    }

    /// The daemon sends the goodbye on its own; its status is not awaited.
    fn unregister(&self, fullname: &str) -> Result<(), mdns_sd::Error> {
        ServiceDaemon::unregister(self, fullname).map(drop)
    }
}

/// Why an announcement stopped. The record is named by its place in the
/// set and its service, never by a channel name.
#[derive(Debug, thiserror::Error)]
pub enum AnnounceError {
    #[error("mDNS record {index} ({service}): {source}")]
    Record {
        index: usize,
        service: &'static str,
        source: mdns_sd::Error,
    },
    #[error("mDNS record {index} ({service}) has the name of an earlier one")]
    Duplicate { index: usize, service: &'static str },
    #[error("withdrawing an mDNS record ({service}): {source}")]
    Withdraw {
        service: &'static str,
        source: mdns_sd::Error,
    },
}

/// The device's records in the mDNS daemon: what is registered now, so the
/// next set replaces it without repeating what did not change.
pub struct Announcer<R: Registry> {
    registry: R,
    /// What the registry holds, by lower-case full name.
    live: BTreeMap<String, ServiceRecord>,
}

impl<R: Registry> Announcer<R> {
    pub fn new(registry: R) -> Self {
        Self {
            registry,
            live: BTreeMap::new(),
        }
    }

    pub fn registry(&self) -> &R {
        &self.registry
    }

    /// Makes `recs` the registered set: the records that are gone are
    /// unregistered first, then every new or changed one is registered.
    /// Every record is built before anything is sent, so a record mdns-sd
    /// refuses changes nothing; a failed call leaves its record to the next
    /// announcement.
    pub fn announce(&mut self, recs: &[ServiceRecord]) -> Result<(), AnnounceError> {
        let mut wanted = Vec::with_capacity(recs.len());
        let mut names = HashSet::with_capacity(recs.len());
        for (index, rec) in recs.iter().enumerate() {
            let info = rec.info().map_err(|source| AnnounceError::Record {
                index,
                service: rec.service,
                source,
            })?;
            let name = info.get_fullname().to_lowercase();
            if !names.insert(name.clone()) {
                return Err(AnnounceError::Duplicate {
                    index,
                    service: rec.service,
                });
            }
            wanted.push((index, name, info, rec));
        }
        let gone: Vec<String> = self
            .live
            .keys()
            .filter(|name| !names.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            let service = self.live[&name].service;
            self.registry
                .unregister(&name)
                .map_err(|source| AnnounceError::Withdraw { service, source })?;
            self.live.remove(&name);
        }
        for (index, name, info, rec) in wanted {
            if self.live.get(&name) == Some(rec) {
                continue;
            }
            self.registry
                .register(info)
                .map_err(|source| AnnounceError::Record {
                    index,
                    service: rec.service,
                    source,
                })?;
            self.live.insert(name, rec.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
