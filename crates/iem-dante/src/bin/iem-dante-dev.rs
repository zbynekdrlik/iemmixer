// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors.

//! `iem-dante-dev`: our own Dante device on the Gen 3 box (#72 phase A,
//! plan Task 5), run by systemd as `iem-dante.service`
//! (`scripts/gen3/iem-dante.service`). See `iem-dante-dev --help`.
//!
//! Glue only, Linux only: the config and state files, the interface list
//! and the interface's files (its MAC and speed in `/sys/class/net`, the
//! routes in `/proc/net/route`), the sockets, the mDNS daemon on the one
//! interface, SIGTERM and SIGINT, and the goodbyes. Every decision is in
//! `iem_dante::daemon` (tested and mutated there). Exit codes: 0 after a
//! signal and the goodbyes; 1 when the address is not there (yet: an
//! interface still coming up at boot), the interface's MAC cannot be read,
//! serving failed or the goodbyes were not confirmed, which the unit
//! restarts; 2 for the command line, the config, the state file or an
//! interface without a usable MAC, which it does not
//! (`RestartPreventExitStatus=2`).

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("iem-dante-dev runs on Linux only (Gen 3, #72)");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fmt::Display;
    use std::net::IpAddr;
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use iem_dante::announce::Announcer;
    use iem_dante::daemon::{
        self, BUILD, Command, IfAddr, Options, Sockets, USAGE, VERSION, net_info, parse_args,
        pick_interface, version_line,
    };
    use iem_dante::device::{Device, DeviceConfig};
    use iem_dante::info::{self, Multicast, NetInfo};
    use iem_dante::{arc, cmc};
    use mdns_sd::{DaemonStatus, IfKind, ServiceDaemon};
    use tokio::net::UdpSocket;
    use tokio::signal::unix::{SignalKind, signal};
    use tracing::{error, info};
    use tracing_subscriber::EnvFilter;

    /// How long the mDNS daemon gets to send the goodbyes and stop.
    const GOODBYE_WAIT: Duration = Duration::from_secs(3);

    /// Why the daemon ended badly, and its exit code.
    enum Failure {
        /// The command line, the config or the state file: 2.
        Setup(String),
        /// The address, serving, or the goodbyes: 1.
        Run(String),
    }

    fn setup(e: impl Display) -> Failure {
        Failure::Setup(e.to_string())
    }

    fn run_failed(e: impl Display) -> Failure {
        Failure::Run(e.to_string())
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let opts = match parse_args(&args) {
            Ok(Command::Version) => {
                println!("{}", version_line());
                return ExitCode::SUCCESS;
            }
            Ok(Command::Help) => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Ok(Command::Run(opts)) => opts,
            Err(msg) => {
                eprintln!("iem-dante-dev: {msg}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
        match run(&opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(Failure::Setup(e)) => {
                error!("{e}");
                ExitCode::from(2)
            }
            Err(Failure::Run(e)) => {
                error!("{e}");
                ExitCode::from(1)
            }
        }
    }

    /// The box's IPv4 addresses, by interface.
    fn interfaces() -> std::io::Result<Vec<IfAddr>> {
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

    /// What the info server says about the interface, from its files: the
    /// MAC must be read (exit 1 when it cannot be, 2 when the interface has
    /// none); the speed and the routes may be missing (a link that is
    /// down, no route table): zeros.
    fn interface_info(ifa: &IfAddr) -> Result<NetInfo, Failure> {
        let sys = format!("/sys/class/net/{}", ifa.name);
        let mac = std::fs::read_to_string(format!("{sys}/address"))
            .map_err(|e| run_failed(format!("the interface's MAC address: {e}")))?;
        let speed = std::fs::read_to_string(format!("{sys}/speed")).ok();
        let routes = std::fs::read_to_string("/proc/net/route").ok();
        net_info(ifa, &mac, speed.as_deref(), routes.as_deref()).map_err(setup)
    }

    fn run(opts: &Options) -> Result<(), Failure> {
        let path = opts.config.display();
        let text =
            std::fs::read_to_string(&opts.config).map_err(|e| setup(format!("{path}: {e}")))?;
        let cfg = DeviceConfig::from_toml(&text).map_err(|e| setup(format!("{path}: {e}")))?;
        let device = Device::load(cfg).map_err(setup)?;
        // An address not there may come (an interface still coming up at
        // boot): exit 1, which the unit retries.
        let ifa =
            pick_interface(&opts.place, &interfaces().map_err(run_failed)?).map_err(run_failed)?;
        let net = interface_info(&ifa)?;
        info!(
            version = VERSION,
            build = BUILD,
            rx = device.cfg.rx.len(),
            tx = device.cfg.tx.len(),
            rate = device.cfg.sample_rate,
            encoding = device.cfg.encoding,
            link_mbit_s = net.link_speed,
            gateway = !net.gateway.is_unspecified(),
            "iem-dante-dev starting (no PTP, no audio)"
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(run_failed)?;
        let mdns = ServiceDaemon::new().map_err(|e| run_failed(format!("the mDNS daemon: {e}")))?;
        let mut announcer = Announcer::new(mdns);
        let served = rt.block_on(serve(device, &mut announcer, net));
        // The goodbyes whatever ended serving: no record outlives the daemon.
        let goodbye = goodbye(announcer.registry());
        served?;
        goodbye
    }

    async fn serve(
        device: Device,
        announcer: &mut Announcer<ServiceDaemon>,
        net: NetInfo,
    ) -> Result<(), Failure> {
        let ip = net.ip;
        let mut term = signal(SignalKind::terminate()).map_err(run_failed)?;
        let mut int = signal(SignalKind::interrupt()).map_err(run_failed)?;
        let bind = |port: u16, what: &'static str| async move {
            UdpSocket::bind((ip, port))
                .await
                .map_err(|e| run_failed(format!("{what} on UDP {port}: {e}")))
        };
        let sockets = Sockets {
            arc: bind(arc::PORT, "ARC").await?,
            cmc: bind(cmc::PORT, "CMC").await?,
            // Bound to the address, as inferno's: the info multicasts leave
            // by its interface.
            info: bind(info::REQUEST_PORT, "the info server").await?,
        };
        // Only the served address's interface, IPv4: no other network sees
        // the records (mdns-sd applies the selections in order).
        let mdns = announcer.registry();
        mdns.disable_interface(IfKind::All)
            .and_then(|()| mdns.enable_interface(IfKind::Addr(IpAddr::V4(ip))))
            .map_err(|e| run_failed(format!("the mDNS interface: {e}")))?;
        info!(
            arc = arc::PORT,
            cmc = cmc::PORT,
            info = info::REQUEST_PORT,
            info_group_port = info::INFO_PORT,
            heartbeat_port = info::HEARTBEAT_PORT,
            "listening on the interface's address"
        );
        let stop = async move {
            tokio::select! {
                _ = term.recv() => info!("SIGTERM: stopping"),
                _ = int.recv() => info!("SIGINT: stopping"),
            }
        };
        daemon::serve(
            sockets,
            Arc::new(Mutex::new(device)),
            announcer,
            net,
            Multicast::DANTE,
            stop,
        )
        .await
        .map_err(run_failed)
    }

    /// Shuts the mDNS daemon down, which withdraws every record (goodbyes)
    /// before it says so; waits at most GOODBYE_WAIT for that.
    fn goodbye(mdns: &ServiceDaemon) -> Result<(), Failure> {
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
}
