// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The `iem-dante-dev` daemon (#72 phase A, plan Task 5): its command line,
//! the address it serves on, and the loop that serves the device.
//!
//! - [`parse_args`]: `--config <path>` and one of `--iface <name>` or `--ip
//!   <IPv4>`; `--version` ([`version_line`]: the workspace version and the
//!   build's commit, CI's `GITHUB_SHA`, "local" otherwise).
//! - [`pick_ipv4`]: the one IPv4 address the device serves on, from the
//!   box's interface list: ARC and CMC bind it, the mDNS records point at
//!   it and are sent on its interface only.
//! - [`serve`]: announces the device's records, answers ARC (UDP 4440) and
//!   CMC (UDP 8800) and announces again after every saved rename, until
//!   its stop future ends (the binary's SIGTERM or SIGINT). The binary then
//!   shuts the mDNS daemon down, which sends the goodbyes.
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

Our own Dante device (Gen 3 phase A, #72): ARC on UDP 4440 and CMC on UDP
8800 at the interface's IPv4 address, its mDNS records on that interface
only, renames saved to the config's state file. SIGTERM or SIGINT withdraws
the records (mDNS goodbyes), then it exits 0. It never sends PTP or audio.";

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
}

/// The address the device serves on: the named interface's one IPv4
/// address, or the given address when an interface holds it. An interface
/// with no or several IPv4 addresses, an address no interface holds, and
/// a loopback address are refused (the errors name neither).
pub fn pick_ipv4(place: &Place, addrs: &[IfAddr]) -> Result<Ipv4Addr, String> {
    let ip = match place {
        Place::Iface(name) => {
            let mine: Vec<Ipv4Addr> = addrs
                .iter()
                .filter(|a| &a.name == name)
                .map(|a| a.ip)
                .collect();
            match mine[..] {
                [ip] => ip,
                [] => return Err("no such interface, or it has no IPv4 address".into()),
                _ => {
                    return Err(format!(
                        "the interface has {} IPv4 addresses: name one with --ip",
                        mine.len()
                    ));
                }
            }
        }
        Place::Ip(ip) => {
            if !addrs.iter().any(|a| a.ip == *ip) {
                return Err("no interface of this box holds the address".into());
            }
            *ip
        }
    };
    refuse_unreachable(ip)?;
    Ok(ip)
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

/// Serves the device at `ip` until `stop` ends: announces its records
/// first, then answers ARC on `arc_sock` and CMC on `cmc_sock`, and
/// announces again after every rename the ARC server saved. A failed
/// announcement, or an ARC or CMC server that stops, ends it with an error
/// (the service restarts the daemon; the state file holds every saved
/// rename). The caller withdraws the records after it returns.
pub async fn serve<R: Registry>(
    arc_sock: UdpSocket,
    cmc_sock: UdpSocket,
    shared: Arc<Mutex<Device>>,
    announcer: &mut Announcer<R>,
    ip: Ipv4Addr,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let changed = Arc::new(Notify::new());
    let n = announce_device(&shared, announcer, ip)?;
    info!(records = n, "mDNS records announced");
    let arc = arc::serve(arc_sock, Arc::clone(&shared), Arc::clone(&changed));
    let cmc = cmc::serve_cmc(cmc_sock, ip);
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
