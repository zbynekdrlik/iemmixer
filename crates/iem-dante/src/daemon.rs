// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The `iem-dante-dev` daemon (#72 phase A, plan Task 5): its command line,
//! the address it serves on, and the loop that serves the device.
//!
//! - [`parse_args`]: `--config <path>` and one of `--iface <name>` or `--ip
//!   <IPv4>`; `--version` ([`version_line`]: the workspace version and the
//!   build's commit, CI's `GITHUB_SHA`, "local" otherwise); `clock
//!   --seconds <n>` (phase B, plan Task 2): the media clock's polls of
//!   dantesync, one JSON line each ([`ClockLines`]), serving nothing.
//! - [`pick_interface`]: the interface and its one IPv4 address the device
//!   serves on, from the box's interface list: ARC, CMC and the info
//!   server bind the address, the mDNS records point at it and are sent on
//!   its interface only.
//! - [`net_info`]: what the info server's network info says about that
//!   interface, from the box's files the binary reads (its MAC and speed
//!   in `/sys/class/net/<iface>/`, the routes in `/proc/net/route`).
//! - [`serve`]: announces the device's records, answers ARC (UDP 4440),
//!   CMC (UDP 8800), the info requests (UDP 8700, [`crate::info`]) and
//!   flow control (UDP 4455, [`crate::flows::server`]), beats every second,
//!   runs the TX and RX threads and our RX's manager ([`media`]), serves
//!   the control socket ([`crate::ctl`]), and announces again after every
//!   saved rename, until its stop future ends (the binary's SIGTERM or
//!   SIGINT). It then stops every RX flow at its transmitter, serving flow
//!   control until the stops are done (a flow from our own TX is stopped at
//!   our own server), and ends the threads; the binary shuts the mDNS
//!   daemon down, which sends the goodbyes.
//!
//! The device sends no PTP. Logs name counts, ports, flow slots and
//! channel numbers, never a name or an address: the device and channel
//! names are the ops repository's (P6).

pub mod media;

use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::announce::{AnnounceError, Announcer, Registry, records};
use crate::clock::{Event, Polled, Sample};
use crate::device::Device;
use crate::flows::client::Client;
use crate::info::{Changes, Identity, Multicast, Multicaster, NetInfo};
use crate::subscribe::TICK;
use crate::subscribe::directory::Network;
use crate::subscribe::resolve::{Browse, BrowseError, Own};
use crate::{arc, cmc, flows, info};
use media::{Driver, Media, THREADS_WAIT};

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
                     [--control <socket path>]
       iem-dante-dev ctl [--socket <path>] <request>
       iem-dante-dev clock --seconds <n>
       iem-dante-dev --version

Our own Dante device (Gen 3, #72): ARC on UDP 4440, CMC on UDP 8800, the
info requests on UDP 8700 and flow control on UDP 4455 at the interface's
IPv4 address (info replies to 224.0.0.231:8702, a heartbeat every second to
224.0.0.233:8708), its mDNS records on that interface only, renames and RX
subscriptions saved to the config's state file. It sends the TX flows
receivers ask for and asks transmitters for its subscribed RX channels, on
PTP time read from dantesync (127.0.0.1:31900); it never sends PTP. Its
control socket (default /run/iem-dante/control.sock) takes one request
line a connection. SIGTERM or SIGINT stops every RX flow at its
transmitter, ends the media threads and withdraws the records (mDNS
goodbyes), then it exits 0.

ctl: sends one request to the control socket and prints the answer line:
  status                    the device's numbers, one JSON line
  source <tx> silence|counter|sine <hz> <dbfs>|echo <rx> <samples>
                            what TX channel tx (from 1) sends
  check <rx> off|counter|sine <hz>
                            which checker reads RX channel rx (from 1)
Exit 0 for ok or the status, 1 for an error answer or no socket.

clock: polls dantesync's time server on the box (127.0.0.1:31900) n times,
a second apart, as the device's media clock does, and prints one JSON line
a poll, numbers only: t_s (seconds from the first poll), ready (1 once the
clock has a k), k_ns (this poll's k, null when the poll gave none), dk_ns
(its change since the line before), off_ns (this poll's k less the
anchored one), re_anchors (so far), re_anchor_ns (this poll's re-anchor,
else null), holdover (1 when this poll kept k without a usable reading),
sample (0 usable, 1 dantesync not locked, 2 no date offset, 3 a date step
within 2 s, 4 a date slew, 5 no reply), locked, mode and freq_ppb
(dantesync's, null without a reply). It serves nothing and never opens
UDP 319 or 320.";

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
    /// The control socket ([`crate::ctl::SOCKET`] unless `--control`).
    pub control: PathBuf,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    /// `clock --seconds <n>`: n polls of dantesync, a line each.
    Clock {
        seconds: u32,
    },
    /// `ctl [--socket <path>] <request>`: one request to the control
    /// socket, its words joined by spaces.
    Ctl {
        socket: PathBuf,
        request: String,
    },
    Version,
    Help,
}

/// The command line (without the program's name). `--version` and
/// `--help` win wherever they stand; anything else is a usage error.
/// `clock` first selects the clock's polls ([`Command::Clock`]), `ctl` a
/// control request ([`Command::Ctl`]).
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    match args.split_first() {
        Some((first, rest)) if first == "clock" => return parse_clock(rest),
        Some((first, rest)) if first == "ctl" => return parse_ctl(rest),
        _ => {}
    }
    let mut config = None;
    let mut iface = None;
    let mut ip = None;
    let mut control = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--config" => &mut config,
            "--iface" => &mut iface,
            "--ip" => &mut ip,
            "--control" => &mut control,
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
    let control = match control {
        Some(path) if path.is_empty() => return Err("--control needs a path".into()),
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(crate::ctl::SOCKET),
    };
    Ok(Command::Run(Options {
        config: PathBuf::from(config),
        place,
        control,
    }))
}

/// `ctl`'s own arguments: `--socket <path>` first if at all, then the
/// request's words (at least one). `--help` first is the usage.
fn parse_ctl(args: &[String]) -> Result<Command, String> {
    let (socket, words) = match args {
        [first, ..] if first == "--help" || first == "-h" => return Ok(Command::Help),
        [flag, path, words @ ..] if flag == "--socket" && !path.is_empty() => {
            (PathBuf::from(path), words)
        }
        [flag, ..] if flag == "--socket" => return Err("--socket needs a path".into()),
        words => (PathBuf::from(crate::ctl::SOCKET), words),
    };
    if words.is_empty() {
        return Err("ctl: the request is missing (status, source or check)".into());
    }
    Ok(Command::Ctl {
        socket,
        request: words.join(" "),
    })
}

/// `clock`'s own arguments: `--seconds <n>`, n from 1.
fn parse_clock(args: &[String]) -> Result<Command, String> {
    let mut seconds = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--seconds" => {
                let Some(value) = it.next() else {
                    return Err("--seconds needs a value".into());
                };
                if seconds.replace(value).is_some() {
                    return Err("--seconds given twice".into());
                }
            }
            other => return Err(format!("clock: unknown argument {other:?}")),
        }
    }
    let text = seconds.ok_or("clock: --seconds is missing")?;
    match text.parse::<u32>() {
        Ok(seconds) if seconds > 0 => Ok(Command::Clock { seconds }),
        _ => Err(format!("--seconds {text:?} is not a whole number from 1")),
    }
}

/// The lines of `iem-dante-dev clock`: one JSON object a poll, numbers
/// only ([`USAGE`] names the fields). Keeps the line before's k and the
/// re-anchors so far.
#[derive(Debug, Default, Clone)]
pub struct ClockLines {
    last_k: Option<i64>,
    re_anchors: u32,
}

impl ClockLines {
    pub fn new() -> Self {
        Self::default()
    }

    /// The line of poll `p`, `t_s` seconds after the first.
    pub fn line(&mut self, t_s: u32, p: &Polled) -> String {
        let k = match p.sample {
            Sample::Usable { k_ns } => Some(k_ns),
            _ => None,
        };
        let dk = k.zip(self.last_k).map(|(k, last)| k.wrapping_sub(last));
        let off = k.zip(p.k_ns).map(|(k, anchored)| k.wrapping_sub(anchored));
        self.last_k = k;
        let re_anchor = match p.event {
            Event::ReAnchored { delta_ns } => {
                self.re_anchors += 1;
                Some(delta_ns)
            }
            _ => None,
        };
        let r = p.reading;
        format!(
            "{{\"t_s\":{t_s},\"ready\":{},\"k_ns\":{},\"dk_ns\":{},\"off_ns\":{},\
             \"re_anchors\":{},\"re_anchor_ns\":{},\"holdover\":{},\"sample\":{},\
             \"locked\":{},\"mode\":{},\"freq_ppb\":{}}}",
            u8::from(p.k_ns.is_some()),
            json_number(k),
            json_number(dk),
            json_number(off),
            self.re_anchors,
            json_number(re_anchor),
            u8::from(p.event == Event::Holdover),
            sample_code(p.sample),
            json_number(r.map(|r| u8::from(r.locked))),
            json_number(r.map(|r| r.mode)),
            json_number(r.map(|r| r.freq_ppb)),
        )
    }
}

/// A sample's number in the clock's lines.
fn sample_code(s: Sample) -> u8 {
    match s {
        Sample::Usable { .. } => 0,
        Sample::NotLocked => 1,
        Sample::NoDateState => 2,
        Sample::StepNear => 3,
        Sample::Slewing => 4,
        Sample::NoReply => 5,
    }
}

/// A number, or `null`.
fn json_number<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".to_string(), |v| v.to_string())
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

/// The fastest link the network info names, in Mbit/s (inferno's clamp).
const MAX_LINK_SPEED: i64 = 10_000;
/// `/proc/net/route` flags: the route is up, and goes through a gateway.
const RTF_UP: u32 = 0x0001;
const RTF_GATEWAY: u32 = 0x0002;

/// The interface's MAC from its `/sys/class/net/<iface>/address` text
/// (`aa:bb:cc:dd:ee:ff`, either case); `None` for anything else, and for
/// all zeros (an interface without a hardware address).
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(':');
    for byte in &mut mac {
        let part = parts.next()?;
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    if parts.next().is_some() || mac == [0; 6] {
        return None;
    }
    Some(mac)
}

/// The link speed in Mbit/s from `/sys/class/net/<iface>/speed`: 0 when
/// it is unknown (the kernel's -1, no file while the link is down, or
/// anything else), at most 10 000, as inferno reports it.
pub fn link_speed(text: Option<&str>) -> u16 {
    let mbit = text.and_then(|t| t.trim().parse::<i64>().ok()).unwrap_or(0);
    u16::try_from(mbit.clamp(0, MAX_LINK_SPEED)).unwrap_or(0)
}

/// The interface's default gateway from `/proc/net/route`: the first
/// default route of `ifa` (destination and mask 0, up, through a gateway)
/// whose gateway lies in the interface's subnet, as inferno takes the
/// interface's gateway; 0.0.0.0 for none (a Dante network often has none).
pub fn default_gateway(routes: &str, ifa: &IfAddr) -> Ipv4Addr {
    let subnet = |a: Ipv4Addr| a.to_bits() & ifa.netmask.to_bits();
    routes
        .lines()
        .filter_map(|line| default_route(line, &ifa.name))
        .find(|gateway| subnet(*gateway) == subnet(ifa.ip))
        .unwrap_or(Ipv4Addr::UNSPECIFIED)
}

/// The gateway of `line` when it is a default route of interface `name`
/// that is up and goes through a gateway. The kernel prints the addresses
/// as the hex of their bytes read in the host's order.
fn default_route(line: &str, name: &str) -> Option<Ipv4Addr> {
    // Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [iface, destination, gateway, flags, _, _, _, mask, ..] = fields[..] else {
        return None;
    };
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    let flags = hex(flags)?;
    // Each flag on its own: a mask joining the two bits with `|` gave
    // cargo-mutants an equivalent `^` (0x1 ^ 0x2 is 0x1 | 0x2).
    let is_default = iface == name
        && hex(destination)? == 0
        && hex(mask)? == 0
        && flags & RTF_UP != 0
        && flags & RTF_GATEWAY != 0;
    if !is_default {
        return None;
    }
    Some(Ipv4Addr::from(hex(gateway)?.to_ne_bytes()))
}

/// What the info server's network info says about `ifa`, from the text of
/// its MAC file, its speed file and the route table (the binary reads
/// them; the speed and the routes may be missing). An interface without a
/// usable MAC is refused.
pub fn net_info(
    ifa: &IfAddr,
    mac: &str,
    speed: Option<&str>,
    routes: Option<&str>,
) -> Result<NetInfo, String> {
    let mac = parse_mac(mac).ok_or("the interface has no usable MAC address")?;
    Ok(NetInfo {
        mac,
        ip: ifa.ip,
        netmask: ifa.netmask,
        gateway: routes.map_or(Ipv4Addr::UNSPECIFIED, |r| default_gateway(r, ifa)),
        link_speed: link_speed(speed),
    })
}

/// The mDNS host the device's records point at: its name's `.local.` host
/// (a device name is a valid host label, [`crate::device::valid_device_name`]).
pub fn host_of(device_name: &str) -> String {
    format!("{device_name}.local.")
}

/// Whether a control answer line (`iem-dante-dev ctl`'s) says the request
/// was done: `ok` or the status, not `error: …` (nor nothing at all).
pub fn ctl_succeeded(answer: &str) -> bool {
    let answer = answer.trim_end();
    answer == "ok" || answer.starts_with('{')
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
    #[error("the flow-control server stopped: {0}")]
    Flows(io::Error),
    #[error("the control socket stopped: {0}")]
    Control(io::Error),
    #[error("the media threads did not start: {0}")]
    Threads(io::Error),
    #[error("the channel records' browse did not start: {0}")]
    Browse(#[from] BrowseError),
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
/// ([`arc::PORT`]), CMC ([`cmc::PORT`]), the info requests
/// ([`crate::info::REQUEST_PORT`]) and flow control
/// ([`flows::control::PORT`]).
#[derive(Debug)]
pub struct Sockets {
    pub arc: UdpSocket,
    pub cmc: UdpSocket,
    pub info: UdpSocket,
    pub flows: UdpSocket,
}

/// Serves the device at `net.ip` until `stop` ends: announces its records
/// first, then answers ARC, CMC, the info requests and flow control on
/// `sockets` (every datagram counted in `media`'s requests), sends the
/// board and product info, the channel-change notifications and the
/// heartbeat with `media`'s records where `multicast` says (the info
/// server, [`info::serve_info`]), runs `media`'s TX and RX threads and its
/// manager every [`TICK`] ([`Driver`]: the channel records it resolves are
/// our own and those of one mDNS browse, started at the first
/// subscription; a device no channel record names is read through its ARC,
/// [`Network`], whose browses and reader start once a lookup reached
/// them), serves `control` (the control socket's server, the
/// binary's [`crate::ctl::serve_control`]), and announces again after every
/// rename the ARC server saved. A failed announcement or browse, a server
/// that stops, or threads that do not start end it with an error (the
/// service restarts the daemon; the state file holds every saved rename and
/// subscription). Whatever ended it, every RX flow is then stopped at its
/// transmitter (within [`media::STOP_WAIT`]), our own flow-control server
/// still served meanwhile unless it is what ended (a flow from our own TX,
/// a self-loop, is stopped there and answered), and the threads are ended
/// (waited for [`THREADS_WAIT`]). The caller withdraws the records after
/// it returns.
#[allow(clippy::too_many_arguments)]
pub async fn serve<R: Registry + Browse>(
    sockets: Sockets,
    shared: Arc<Mutex<Device>>,
    announcer: &mut Announcer<R>,
    net: NetInfo,
    multicast: Multicast,
    media: Arc<Media>,
    control: impl Future<Output = io::Result<()>>,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let ip = net.ip;
    let changed = Arc::new(Notify::new());
    let n = announce_device(&shared, announcer, ip)?;
    info!(records = n, "mDNS records announced");
    let identity = {
        let device = shared.lock().unwrap_or_else(PoisonError::into_inner);
        Identity::new(&device.cfg, net)
    };
    let threads = media.spawn_threads().map_err(ServeError::Threads)?;
    let rx_changes = Arc::new(Changes::default());
    let requests = Arc::clone(&media.requests);
    let (mut driver, mut answers) = Driver::new(
        Arc::clone(&media),
        Client::new(media.ip),
        Arc::clone(&rx_changes),
    );
    let arc = arc::serve(
        sockets.arc,
        Arc::clone(&shared),
        Arc::new(media.flows()),
        Arc::clone(&changed),
        Arc::clone(&rx_changes),
        Arc::clone(&requests),
    );
    let cmc = cmc::serve_cmc(sockets.cmc, ip, Arc::clone(&requests));
    let info = info::serve_info(
        sockets.info,
        Multicaster::new(identity),
        multicast,
        Arc::new(media.beat()),
        rx_changes,
        Arc::clone(&requests),
    );
    let flows = flows::server::serve(sockets.flows, Arc::clone(&media.tx), requests);
    // Our own channels resolve from our records (built again after a
    // rename), the network's from one browse of the channel records, and a
    // device that publishes none through its ARC.
    let mut resolver = (Own::default(), Network::default());
    let mut own_stale = true;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tokio::pin!(arc, cmc, info, flows, control, stop);
    let ended = loop {
        tokio::select! {
            biased;
            () = &mut stop => break Ok(()),
            ended = &mut arc => break Err(ServeError::Arc(ended_error(ended))),
            ended = &mut cmc => break Err(ServeError::Cmc(ended_error(ended))),
            ended = &mut info => break Err(ServeError::Info(ended_error(ended))),
            ended = &mut flows => break Err(ServeError::Flows(ended_error(ended))),
            ended = &mut control => break Err(ServeError::Control(ended_error(ended))),
            () = changed.notified() => {
                own_stale = true;
                match announce_device(&shared, announcer, ip) {
                    Ok(n) => info!(records = n, "mDNS records announced again after a rename"),
                    Err(e) => break Err(e.into()),
                }
            }
            Some((flow, answer)) = answers.recv() => driver.answered(flow, answer),
            _ = tick.tick() => {
                if own_stale {
                    let device = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    resolver.0 = Own::new(&device.state, &device.cfg, ip);
                    own_stale = false;
                }
                let subscribed = media
                    .manager
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .subscribed();
                if subscribed && !resolver.1.chan.started() {
                    if let Err(e) = resolver.1.chan.start(announcer.registry()) {
                        break Err(e.into());
                    }
                    info!("mDNS browse of the channel records started");
                }
                let arc = &mut resolver.1.arc;
                if arc.asked() && !arc.started() {
                    if let Err(e) = arc.start(announcer.registry(), ip, Arc::clone(&media.requests)) {
                        break Err(e.into());
                    }
                    info!("mDNS browses of the ARC and flow-control records and the ARC reader started");
                }
                driver.tick(&resolver);
            }
        }
    };
    // Whatever ended serving: our RX flows end at their transmitters, the
    // media threads end. Our own flow-control server is served until the
    // stops are done (unless it is what ended, or it ends meanwhile): a
    // flow from our own TX (a self-loop) is stopped there and answered.
    let mut flows_served = !matches!(ended, Err(ServeError::Flows(_)));
    let stopped = driver.stop();
    tokio::pin!(stopped);
    loop {
        tokio::select! {
            biased;
            () = &mut stopped => break,
            gone = &mut flows, if flows_served => {
                flows_served = false;
                warn!(
                    error = %ended_error(gone),
                    "the flow-control server stopped during the stop"
                );
            }
        }
    }
    // One still running is left to end, with a warning.
    let _ended = threads.stop(&media, THREADS_WAIT).await;
    ended
}

/// A server's end as an error: they end only with one.
fn ended_error(ended: io::Result<()>) -> io::Error {
    ended
        .err()
        .unwrap_or_else(|| io::Error::other("ended without an error"))
}

#[cfg(test)]
mod tests;
