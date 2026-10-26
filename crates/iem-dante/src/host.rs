// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! The device as a library (Gen 3 phase C, plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md`
//! Task 2): what `iem-dante-dev` ran in its `main`, so the engine can host
//! the device in its own process (Task 3) and the standalone binary stays a
//! thin caller. Linux only, as the binary.
//!
//! - [`prepare`]: the device's setup, nothing served yet: the config and
//!   its state file (a folder it cannot write is found here), the interface
//!   and its address, its facts for the network info, the start line, the
//!   runtime, the four servers' sockets, the media side ([`Media`], with an
//!   engine ring on each TX channel the engine feeds) and the control
//!   socket bound. [`Prepared::media`] and [`Prepared::config`] are what the
//!   engine builds its pump from.
//! - [`Prepared::start`]: the mDNS daemon on the interface only, the
//!   signals ([`Signals`]), and the `dante-device` thread: a current-thread
//!   tokio runtime running the media clock's poller and
//!   [`daemon::serve`] until it is stopped, then the mDNS goodbyes.
//! - [`Host`]: the running device: [`Host::ended`] (serving ended by
//!   itself, and why), [`Host::stop`] (asks serving to end and waits a
//!   bounded time; one still running is a warning, never killed),
//!   [`Host::wait`] (the standalone binary's wait for a signal's end).
//!
//! [`prepare`] and [`Prepared::start`] use the device's own runtime on the
//! caller's thread (its sockets are bound and its signals registered in
//! it): call them, and drop a [`Prepared`] that is never started, outside
//! any tokio runtime.
//!
//! Errors keep the binary's exit codes ([`exit_code`]): 2 for the command
//! line, the config, the state file (also one whose folder cannot be
//! written) or an interface without a usable MAC, which the unit does not
//! restart (`RestartPreventExitStatus=2`); 1 for an address not there
//! (yet), a MAC file that cannot be read, a socket not bound, serving that
//! failed or goodbyes not confirmed, which it restarts. Logs name counts,
//! ports and channel numbers, never a name or an address.

use std::fmt::Display;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mdns_sd::{DaemonStatus, IfKind, ServiceDaemon};
use tokio::net::{UdpSocket, UnixListener};
use tokio::runtime::Runtime;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::announce::Announcer;
use crate::clock::{ClockStats, MonoClock, monotonic_ns, poll_dantesync, realtime_ns};
use crate::daemon::media::{Clock, Media};
use crate::daemon::{self, BUILD, IfAddr, Options, ServeError, Sockets, VERSION};
use crate::device::{Device, DeviceConfig, DeviceState};
use crate::info::{self, Multicast, NetInfo};
use crate::{arc, cmc, ctl, flows};

/// How long the mDNS daemon gets to send the goodbyes and stop.
pub const GOODBYE_WAIT: Duration = Duration::from_secs(3);

/// Why the device could not run, or ended badly; its exit code is
/// [`exit_code`]'s.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The config, the state file or the interface's MAC: exit 2 (not
    /// restarted).
    #[error("{0}")]
    Setup(String),
    /// The address, a socket, serving or the goodbyes: exit 1 (restarted).
    #[error("{0}")]
    Run(String),
}

/// The exit code of `e`: 2 for [`HostError::Setup`], 1 for
/// [`HostError::Run`] (phase B's).
pub fn exit_code(e: &HostError) -> u8 {
    match e {
        HostError::Setup(_) => 2,
        HostError::Run(_) => 1,
    }
}

fn setup(e: impl Display) -> HostError {
    HostError::Setup(e.to_string())
}

fn run_failed(e: impl Display) -> HostError {
    HostError::Run(e.to_string())
}

/// What SIGTERM and SIGINT do while the device serves.
#[derive(Debug, Clone)]
pub enum Signals {
    /// End serving (the standalone device: the unit's stop).
    Stop,
    /// Only set the flag: the host (the engine) decides when to stop,
    /// through [`Host::stop`].
    Report(Arc<AtomicBool>),
}

/// The device's config read from `path`: a file that cannot be read or a
/// config that does not hold is [`HostError::Setup`] naming the path (the
/// config's own errors name a line, never a value).
pub fn read_config(path: &Path) -> Result<DeviceConfig, HostError> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| setup(format!("{shown}: {e}")))?;
    DeviceConfig::from_toml(&text).map_err(|e| setup(format!("{shown}: {e}")))
}

/// The device of `cfg` with its state file loaded, whose folder must be
/// writable now (found at the start, not at the first rename's save, which
/// would be refused): [`HostError::Setup`] otherwise.
pub fn load_device(cfg: DeviceConfig) -> Result<Device, HostError> {
    let device = Device::load(cfg).map_err(setup)?;
    DeviceState::check_writable(&device.cfg.state_file).map_err(|e| {
        setup(format!(
            "the state file cannot be written ({e}): a rename could not be saved; \
             the folder must be writable by the service's user (StateDirectory=iem-dante)"
        ))
    })?;
    Ok(device)
}

/// The box's IPv4 addresses, by interface.
pub fn interfaces() -> std::io::Result<Vec<IfAddr>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(IfAddr {
                name: i.name,
                ip: v4.ip,
                netmask: v4.netmask,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect())
}

/// What the info server says about the interface, from its files: the MAC
/// must be read ([`HostError::Run`] when it cannot be, [`HostError::Setup`]
/// when the interface has none); the speed and the routes may be missing
/// (a link that is down, no route table): zeros.
pub fn interface_info(ifa: &IfAddr) -> Result<NetInfo, HostError> {
    let sys = format!("/sys/class/net/{}", ifa.name);
    let mac = std::fs::read_to_string(format!("{sys}/address"))
        .map_err(|e| run_failed(format!("the interface's MAC address: {e}")))?;
    let speed = std::fs::read_to_string(format!("{sys}/speed")).ok();
    let routes = std::fs::read_to_string("/proc/net/route").ok();
    daemon::net_info(ifa, &mac, speed.as_deref(), routes.as_deref()).map_err(setup)
}

/// The TX flows' cookies: any start differing between runs.
fn seed() -> u64 {
    (realtime_ns() as u64) ^ ((monotonic_ns() as u64) << 21)
}

/// The four servers' sockets, each bound to `ip` (in `rt`).
fn bind_sockets(rt: &Runtime, ip: std::net::Ipv4Addr) -> Result<Sockets, HostError> {
    rt.block_on(async {
        let bind = |port: u16, what: &'static str| async move {
            UdpSocket::bind((ip, port))
                .await
                .map_err(|e| run_failed(format!("{what} on UDP {port}: {e}")))
        };
        Ok(Sockets {
            arc: bind(arc::PORT, "ARC").await?,
            cmc: bind(cmc::PORT, "CMC").await?,
            // Bound to the address, as inferno's: the info multicasts
            // leave by its interface.
            info: bind(info::REQUEST_PORT, "the info server").await?,
            flows: bind(flows::control::PORT, "flow control").await?,
        })
    })
}

/// The device set up and bound, nothing served yet (see the module).
pub struct Prepared {
    device: Device,
    net: NetInfo,
    rt: Runtime,
    sockets: Sockets,
    /// The media clock the poller keeps (the media side reads it).
    mono: Arc<MonoClock>,
    media: Arc<Media>,
    listener: UnixListener,
}

/// The device of `opts` set up, nothing served yet: the config, its state
/// file (writable), the interface and its address, its network info, the
/// runtime, the sockets of ARC, CMC, the info server and flow control on the
/// address, the media side on it with an engine ring on each TX channel
/// index (from 0) in `engine_tx` (the standalone device passes none; an
/// index the device lacks is [`HostError::Setup`]), and the control socket
/// at `opts.control`. Glue: every decision is [`read_config`]'s,
/// [`load_device`]'s, [`daemon::pick_interface`]'s, [`interface_info`]'s
/// and [`Media::new`]'s.
pub fn prepare(opts: &Options, engine_tx: &[usize]) -> Result<Prepared, HostError> {
    let device = load_device(read_config(&opts.config)?)?;
    if let Some(&tx) = engine_tx.iter().find(|&&tx| tx >= device.cfg.tx.len()) {
        return Err(setup(format!(
            "the engine's TX channel {} is past the device's {} TX channels",
            tx + 1,
            device.cfg.tx.len()
        )));
    }
    // An address not there may come (an interface still coming up at
    // boot): exit 1, which the unit retries.
    let ifa = daemon::pick_interface(&opts.place, &interfaces().map_err(run_failed)?)
        .map_err(run_failed)?;
    let net = interface_info(&ifa)?;
    info!(
        version = VERSION,
        build = BUILD,
        rx = device.cfg.rx.len(),
        tx = device.cfg.tx.len(),
        engine_tx = engine_tx.len(),
        rate = device.cfg.sample_rate,
        encoding = device.cfg.encoding,
        latency_ns = device.cfg.latency_ns,
        link_mbit_s = net.link_speed,
        gateway = !net.gateway.is_unspecified(),
        "Dante device starting (flows on PTP time from dantesync; no PTP sent)"
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(run_failed)?;
    let sockets = bind_sockets(&rt, net.ip)?;
    // The media clock: PTP time from dantesync on the box, polled once a
    // second for the device's life (from the start).
    let mono = Arc::new(MonoClock::new());
    let clock = Clock {
        ptp: mono.clone(),
        freq: Arc::new(AtomicI32::new(0)),
        stats: Arc::new(ClockStats::default()),
    };
    let media = Media::new(&device, net.ip, clock, seed(), engine_tx)
        .map_err(|e| run_failed(format!("the flow sockets: {e}")))?;
    let listener = {
        let _in_rt = rt.enter();
        ctl::bind_control(&opts.control).map_err(|e| {
            run_failed(format!(
                "the control socket {}: {e} (the unit's RuntimeDirectory makes its folder)",
                opts.control.display()
            ))
        })?
    };
    Ok(Prepared {
        device,
        net,
        rt,
        sockets,
        mono,
        media: Arc::new(media),
        listener,
    })
}

/// What the device's thread and its host share.
#[derive(Default)]
struct Control {
    /// Asks serving to end (a permit, so a request before serving waits
    /// for it is kept).
    stop: Notify,
    /// Why serving ended by itself; set once serving returned (its RX
    /// flows stopped, its media threads ended: within 2 s of the end),
    /// before the goodbyes.
    ended: OnceLock<String>,
}

/// Serving's end as the thread reports it: an error is kept as the reason
/// serving ended by itself ([`Host::ended`]) and is the thread's
/// [`HostError::Run`]; an end that was asked for (a stop, a signal) is
/// none.
fn settle(control: &Control, served: Result<(), ServeError>) -> Result<(), HostError> {
    served.map_err(|e| {
        let why = e.to_string();
        let _first = control.ended.set(why.clone());
        HostError::Run(why)
    })
}

impl Prepared {
    /// The media side: the engine's pump reads its RX rings and writes its
    /// TX side's engine rings; [`Media::attach_engine`] shows the pump on
    /// the control socket.
    pub fn media(&self) -> Arc<Media> {
        Arc::clone(&self.media)
    }

    /// The device's config (the engine maps its channels against it).
    pub fn config(&self) -> &DeviceConfig {
        &self.device.cfg
    }

    /// Starts serving: the signals as `signals` says, the mDNS daemon on
    /// the address's interface only, and the `dante-device` thread (see
    /// the module). Glue: the mDNS daemon, the signals and the thread.
    pub fn start(self, signals: Signals) -> Result<Host, HostError> {
        let Prepared {
            device,
            net,
            rt,
            sockets,
            mono,
            media,
            listener,
        } = self;
        let watch = {
            let _in_rt = rt.enter();
            Watch::new(signals)?
        };
        let mdns = ServiceDaemon::new().map_err(|e| run_failed(format!("the mDNS daemon: {e}")))?;
        // Only the served address's interface, IPv4: no other network sees
        // the records (mdns-sd applies the selections in order).
        let selected = mdns
            .disable_interface(IfKind::All)
            .and_then(|()| mdns.enable_interface(IfKind::Addr(IpAddr::V4(net.ip))));
        if let Err(e) = selected {
            let _withdrawn = goodbye(&mdns);
            return Err(run_failed(format!("the mDNS interface: {e}")));
        }
        let control = Arc::new(Control::default());
        let served = Served {
            device,
            net,
            sockets,
            mono,
            media,
            listener,
        };
        let kept = mdns.clone();
        let theirs = Arc::clone(&control);
        let thread = std::thread::Builder::new()
            .name("dante-device".to_string())
            .spawn(move || device_thread(rt, served, Announcer::new(mdns), &theirs, watch));
        match thread {
            Ok(thread) => Ok(Host { control, thread }),
            Err(e) => {
                let _withdrawn = goodbye(&kept);
                Err(run_failed(format!("the device thread: {e}")))
            }
        }
    }
}

/// What serving takes from [`Prepared`].
struct Served {
    device: Device,
    net: NetInfo,
    sockets: Sockets,
    mono: Arc<MonoClock>,
    media: Arc<Media>,
    listener: UnixListener,
}

/// The signals as registered (in the device's runtime).
enum Watch {
    Stop {
        term: Signal,
        int: Signal,
    },
    Report {
        term: Signal,
        int: Signal,
        flag: Arc<AtomicBool>,
    },
}

impl Watch {
    /// Registers SIGTERM and SIGINT (inside the runtime).
    fn new(signals: Signals) -> Result<Watch, HostError> {
        let term = signal(SignalKind::terminate()).map_err(run_failed)?;
        let int = signal(SignalKind::interrupt()).map_err(run_failed)?;
        Ok(match signals {
            Signals::Stop => Watch::Stop { term, int },
            Signals::Report(flag) => Watch::Report { term, int, flag },
        })
    }

    /// The future that ends serving: a signal (for [`Signals::Stop`]) or
    /// the host's stop. For [`Signals::Report`] a task sets the flag at
    /// each signal while the device serves. Glue: tokio's signals.
    async fn stop(self, control: &Control) {
        match self {
            Watch::Stop { mut term, mut int } => {
                tokio::select! {
                    _ = term.recv() => info!("SIGTERM: stopping"),
                    _ = int.recv() => info!("SIGINT: stopping"),
                    () = control.stop.notified() => info!("the device's stop was asked"),
                }
            }
            Watch::Report { term, int, flag } => {
                tokio::spawn(report(term, int, flag));
                control.stop.notified().await;
                info!("the device's stop was asked");
            }
        }
    }
}

/// Sets `flag` at every SIGTERM or SIGINT (the host stops the device).
async fn report(mut term: Signal, mut int: Signal, flag: Arc<AtomicBool>) {
    loop {
        let which = tokio::select! {
            Some(()) = term.recv() => "SIGTERM",
            Some(()) = int.recv() => "SIGINT",
            else => return,
        };
        info!(
            signal = which,
            "a stop was asked: the host stops the device"
        );
        flag.store(true, Ordering::Release);
    }
}

/// The `dante-device` thread: serving in the runtime until stopped, then
/// the goodbyes whatever ended serving (no record outlives the device).
/// Glue: the runtime and the mDNS daemon.
fn device_thread(
    rt: Runtime,
    served: Served,
    mut announcer: Announcer<ServiceDaemon>,
    control: &Control,
    watch: Watch,
) -> Result<(), HostError> {
    let served = rt.block_on(serve_device(served, &mut announcer, control, watch));
    let served = settle(control, served);
    let goodbye = goodbye(announcer.registry());
    served?;
    goodbye
}

/// The media clock's poller, then [`daemon::serve`] until `watch`'s stop.
async fn serve_device(
    s: Served,
    announcer: &mut Announcer<ServiceDaemon>,
    control: &Control,
    watch: Watch,
) -> Result<(), ServeError> {
    tokio::spawn(poll_dantesync(
        s.mono,
        Arc::clone(&s.media.freq),
        Arc::clone(&s.media.clock_stats),
    ));
    info!(
        arc = arc::PORT,
        cmc = cmc::PORT,
        info = info::REQUEST_PORT,
        flows = flows::control::PORT,
        info_group_port = info::INFO_PORT,
        heartbeat_port = info::HEARTBEAT_PORT,
        "listening on the interface's address"
    );
    let ctl_server = ctl::serve_control(s.listener, Arc::clone(&s.media));
    daemon::serve(
        s.sockets,
        Arc::new(Mutex::new(s.device)),
        announcer,
        s.net,
        Multicast::DANTE,
        s.media,
        ctl_server,
        watch.stop(control),
    )
    .await
}

/// Shuts the mDNS daemon down, which withdraws every record (goodbyes)
/// before it says so; waits at most [`GOODBYE_WAIT`] for that.
fn goodbye(mdns: &ServiceDaemon) -> Result<(), HostError> {
    let status = mdns
        .shutdown()
        .map_err(|e| run_failed(format!("the mDNS shutdown: {e}")))?;
    match status.recv_timeout(GOODBYE_WAIT) {
        Ok(DaemonStatus::Shutdown) => {
            info!("mDNS records withdrawn (goodbyes sent); stopped");
            Ok(())
        }
        Ok(other) => Err(run_failed(format!("the mDNS shutdown answered {other:?}"))),
        Err(e) => Err(run_failed(format!(
            "the mDNS shutdown was not confirmed within {} s: {e}",
            GOODBYE_WAIT.as_secs()
        ))),
    }
}

/// How [`Host::stop`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// Served, stopped, goodbyes sent.
    Done,
    /// The device's thread ended with this error (serving failed before, or
    /// the goodbyes were not confirmed).
    Failed(String),
    /// Still running at the end of the wait: left to end, never killed.
    StillRunning,
}

/// The running device (see the module).
pub struct Host {
    control: Arc<Control>,
    thread: JoinHandle<Result<(), HostError>>,
}

/// The device thread's end when it panicked.
const PANICKED: &str = "the device thread panicked";

impl Host {
    /// Why serving ended by itself (a server that stopped, threads that did
    /// not start, …), once serving returned and before the goodbyes; `None`
    /// while it serves or when it was asked to end.
    pub fn ended(&self) -> Option<String> {
        self.control.ended.get().cloned()
    }

    /// Asks serving to end (every RX flow stopped at its transmitter, the
    /// media threads ended, the goodbyes) and waits up to `within` for the
    /// device's thread: [`Stopped::Done`], [`Stopped::Failed`] with its
    /// error, or [`Stopped::StillRunning`] (a warning; the thread is left to
    /// end, never killed).
    pub fn stop(self, within: Duration) -> Stopped {
        self.control.stop.notify_one();
        let deadline = Instant::now() + within;
        while !self.thread.is_finished() {
            if Instant::now() >= deadline {
                warn!(
                    wait_ms = within.as_millis(),
                    "the device did not end within the wait: left to end"
                );
                return Stopped::StillRunning;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        match self.thread.join() {
            Ok(Ok(())) => Stopped::Done,
            Ok(Err(e)) => Stopped::Failed(e.to_string()),
            Err(_) => Stopped::Failed(PANICKED.to_string()),
        }
    }

    /// Waits for the device's thread to end by itself (a signal under
    /// [`Signals::Stop`], or serving that failed): its result.
    pub fn wait(self) -> Result<(), HostError> {
        self.thread
            .join()
            .unwrap_or_else(|_| Err(HostError::Run(PANICKED.to_string())))
    }
}

#[cfg(test)]
mod tests;
