// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The `iem-dante-dev` daemon (#72 phase A, plan Task 5): its command line,
//! the address it serves on, and the loop that serves the device.
//!
//! - [`parse_args`]: `--config <path>` and one of `--iface <name>` or `--ip
//!   <IPv4>`; `--version` ([`version_line`]: the workspace version and the
//!   build's commit, CI's `GITHUB_SHA`, "local" otherwise).
//! - [`pick_interface`]: the interface and its one IPv4 address the device
//!   serves on, from the box's interface list: ARC, CMC and the info
//!   server bind the address, the mDNS records point at it and are sent on
//!   its interface only.
//! - [`net_info`]: what the info server's network info says about that
//!   interface, from the box's files the binary reads (its MAC and speed
//!   in `/sys/class/net/<iface>/`, the routes in `/proc/net/route`).
//! - [`serve`]: announces the device's records, answers ARC (UDP 4440),
//!   CMC (UDP 8800) and the info requests (UDP 8700, [`crate::info`]),
//!   beats every second, and announces again after every saved rename,
//!   until its stop future ends (the binary's SIGTERM or SIGINT). The
//!   binary then shuts the mDNS daemon down, which sends the goodbyes.
//!
//! The device sends no PTP and no audio (phase A). Logs name counts,
//! ports and channel numbers, never a name or an address: the device and
//! channel names are the ops repository's (P6).

use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tracing::info;

use crate::announce::{AnnounceError, Announcer, Registry, records};
use crate::device::Device;
use crate::info::{Multicast, NetInfo};
use crate::{arc, cmc};

/// The binary's name.
pub const NAME: &str = "iem-dante-dev";
/// The workspace version (`[workspace.package].version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit CI built this from (`GITHUB_SHA`, as the engine and the
/// guard name their builds), "local" for a build outside CI.
pub const BUILD: &str = match option_env!("GITHUB_SHA") {
    Some(sha) => sha,
    None => "local",
};

pub const USAGE: &str = "\
usage: iem-dante-dev --config <path> (--iface <name> | --ip <IPv4 address>)
       iem-dante-dev --version

Our own Dante device (Gen 3 phase A, #72): ARC on UDP 4440, CMC on UDP 8800
and the info requests on UDP 8700 at the interface's IPv4 address (replies
to 224.0.0.231:8702, a heartbeat every second to 224.0.0.233:8708), its
mDNS records on that interface only, renames saved to the config's state
file. SIGTERM or SIGINT withdraws the records (mDNS goodbyes), then it
exits 0. It never sends PTP or audio.";

/// What `--version` prints: the name, the version and the build.
pub fn version_line() -> String {
    format!("{NAME} {VERSION} {BUILD}")
}

/// Where the device serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// The interface's one IPv4 address.
    Iface(String),
    /// This address, which one of the box's interfaces must hold.
    Ip(Ipv4Addr),
}

/// A run's options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub config: PathBuf,
    pub place: Place,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Version,
    Help,
}

/// The command line (without the program's name). `--version` and
/// `--help` win wherever they stand; anything else is a usage error.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut config = None;
    let mut iface = None;
    let mut ip = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--config" => &mut config,
            "--iface" => &mut iface,
            "--ip" => &mut ip,
            other => return Err(format!("unknown argument {other:?}")),
        };
        let Some(value) = it.next() else {
            return Err(format!("{arg} needs a value"));
        };
        if slot.replace(value.clone()).is_some() {
            return Err(format!("{arg} given twice"));
        }
    }
    let config = config.ok_or("--config is missing")?;
    let place = match (iface, ip) {
        (Some(name), None) if !name.is_empty() => Place::Iface(name),
        (Some(_), None) => return Err("--iface needs an interface name".into()),
        (None, Some(text)) => Place::Ip(unicast(&text)?),
        (None, None) => return Err("one of --iface and --ip is missing".into()),
        (Some(_), Some(_)) => return Err("--iface and --ip together: give one".into()),
    };
    Ok(Command::Run(Options {
        config: PathBuf::from(config),
        place,
    }))
}

/// `text` as a unicast IPv4 address the Dante network can reach.
fn unicast(text: &str) -> Result<Ipv4Addr, String> {
    let ip: Ipv4Addr = text
        .parse()
        .map_err(|_| "--ip is not an IPv4 address".to_string())?;
    refuse_unreachable(ip)?;
    Ok(ip)
}

fn refuse_unreachable(ip: Ipv4Addr) -> Result<(), String> {
    if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || ip.is_loopback() {
        return Err("the address is not one the Dante network reaches \
                    (unspecified, broadcast, multicast or loopback)"
            .into());
    }
    Ok(())
}

/// One IPv4 address of one of the box's interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfAddr {
    pub name: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

/// The interface and address the device serves on: the named interface's
/// one IPv4 address, or the given address on the (first) interface that
/// holds it. An interface with no or several IPv4 addresses, an address no
/// interface holds, and a loopback address are refused (the errors name
/// neither).
pub fn pick_interface(place: &Place, addrs: &[IfAddr]) -> Result<IfAddr, String> {
    let picked = match place {
        Place::Iface(name) => {
            let mine: Vec<&IfAddr> = addrs.iter().filter(|a| &a.name == name).collect();
            match mine[..] {
                [one] => one,
                [] => return Err("no such interface, or it has no IPv4 address".into()),
                _ => {
                    return Err(format!(
                        "the interface has {} IPv4 addresses: name one with --ip",
                        mine.len()
                    ));
                }
            }
        }
        Place::Ip(ip) => addrs
            .iter()
            .find(|a| a.ip == *ip)
            .ok_or("no interface of this box holds the address")?,
    };
    refuse_unreachable(picked.ip)?;
    Ok(picked.clone())
}

/// The interface's MAC from its `/sys/class/net/<iface>/address` text
/// (`aa:bb:cc:dd:ee:ff`); `None` for anything else or all zeros.
pub fn parse_mac(_text: &str) -> Option<[u8; 6]> {
    todo!()
}

/// The link speed in Mbit/s from `/sys/class/net/<iface>/speed`, 0 when
/// unknown, at most 10 000 (inferno's).
pub fn link_speed(_text: Option<&str>) -> u16 {
    todo!()
}

/// The interface's default gateway from `/proc/net/route`, 0.0.0.0 for
/// none.
pub fn default_gateway(_routes: &str, _ifa: &IfAddr) -> Ipv4Addr {
    todo!()
}

/// What the info server's network info says about `ifa`.
pub fn net_info(
    _ifa: &IfAddr,
    _mac: &str,
    _speed: Option<&str>,
    _routes: Option<&str>,
) -> Result<NetInfo, String> {
    todo!()
}

/// The mDNS host the device's records point at: its name's `.local.` host
/// (a device name is a valid host label, [`crate::device::valid_device_name`]).
pub fn host_of(device_name: &str) -> String {
    format!("{device_name}.local.")
}

/// Why the daemon stopped serving before it was asked to.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("the ARC server stopped: {0}")]
    Arc(io::Error),
    #[error("the CMC server stopped: {0}")]
    Cmc(io::Error),
    #[error("the info server stopped: {0}")]
    Info(io::Error),
    #[error("announcing the device: {0}")]
    Announce(#[from] AnnounceError),
}

/// Makes the device's records as they are now the announced set; returns
/// how many there are.
pub fn announce_device<R: Registry>(
    shared: &Mutex<Device>,
    announcer: &mut Announcer<R>,
    ip: Ipv4Addr,
) -> Result<usize, AnnounceError> {
    let recs = {
        let device = shared.lock().unwrap_or_else(PoisonError::into_inner);
        records(&device.state, &device.cfg, &host_of(&device.state.name), ip)
    };
    announcer.announce(&recs)?;
    Ok(recs.len())
}

/// The device's sockets, each bound to the served address: ARC
/// ([`arc::PORT`]), CMC ([`cmc::PORT`]) and the info requests
/// ([`crate::info::REQUEST_PORT`]).
#[derive(Debug)]
pub struct Sockets {
    pub arc: UdpSocket,
    pub cmc: UdpSocket,
    pub info: UdpSocket,
}

/// Serves the device at `net.ip` until `stop` ends.
pub async fn serve<R: Registry>(
    sockets: Sockets,
    shared: Arc<Mutex<Device>>,
    announcer: &mut Announcer<R>,
    net: NetInfo,
    _multicast: Multicast,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let ip = net.ip;
    let changed = Arc::new(Notify::new());
    let n = announce_device(&shared, announcer, ip)?;
    info!(records = n, "mDNS records announced");
    let arc = arc::serve(sockets.arc, Arc::clone(&shared), Arc::clone(&changed));
    let cmc = cmc::serve_cmc(sockets.cmc, ip);
    tokio::pin!(arc, cmc, stop);
    loop {
        tokio::select! {
            biased;
            () = &mut stop => return Ok(()),
            ended = &mut arc => return Err(ServeError::Arc(ended_error(ended))),
            ended = &mut cmc => return Err(ServeError::Cmc(ended_error(ended))),
            () = changed.notified() => {
                let n = announce_device(&shared, announcer, ip)?;
                info!(records = n, "mDNS records announced again after a rename");
            }
        }
    }
}

/// A server's end as an error: they end only with one.
fn ended_error(ended: io::Result<()>) -> io::Error {
    ended
        .err()
        .unwrap_or_else(|| io::Error::other("ended without an error"))
}

#[cfg(test)]
mod tests;
