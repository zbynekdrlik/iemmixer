use super::*;

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::mpsc;

use crate::daemon::Place;

const TEST_DEVICE: &str = include_str!("../../../../config/test-dante-device.toml");
/// Every wait's bound, so a mutant that hangs fails fast.
const BOUND: Duration = Duration::from_secs(5);
/// An address no interface of a build box holds (RFC 5737's TEST-NET-1).
const NOWHERE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 77);

#[test]
fn exit_codes_are_phase_b_s() {
    assert_eq!(exit_code(&HostError::Setup("the config".into())), 2);
    assert_eq!(exit_code(&HostError::Run("the address".into())), 1);
    // The error's text is its own, as the binary logged it.
    assert_eq!(HostError::Run("why".into()).to_string(), "why");
    assert_eq!(HostError::Setup("why".into()).to_string(), "why");
}

#[test]
fn a_serving_error_is_ended_with_its_reason() {
    let control = Control::default();
    let served = settle(&control, Err(ServeError::Arc(io::Error::other("gone"))));
    let why = "the ARC server stopped: gone";
    assert_eq!(served, Err(HostError::Run(why.into())));
    assert_eq!(control.ended.get().map(String::as_str), Some(why));
    // An end that was asked for is none.
    let asked = Control::default();
    assert_eq!(settle(&asked, Ok(())), Ok(()));
    assert_eq!(asked.ended.get(), None);
}

// ---- the setup ----

/// The synthetic device's config, its state file in `dir`.
fn config_in(dir: &Path) -> (PathBuf, DeviceConfig) {
    let state = dir.join("state.toml");
    let text = format!(
        "{}\nstate_file = {:?}\n",
        TEST_DEVICE
            .lines()
            .filter(|l| !l.starts_with("state_file"))
            .collect::<Vec<_>>()
            .join("\n"),
        state.display().to_string()
    );
    let path = dir.join("device.toml");
    std::fs::write(&path, &text).unwrap();
    let cfg = DeviceConfig::from_toml(&text).unwrap();
    assert_eq!(cfg.state_file, state);
    (path, cfg)
}

fn options(config: PathBuf, place: Place, dir: &Path) -> Options {
    Options {
        config,
        place,
        control: dir.join("control.sock"),
    }
}

fn setup_error(e: HostError) -> String {
    match e {
        HostError::Setup(why) => why,
        HostError::Run(why) => panic!("a run error, not a setup one: {why}"),
    }
}

fn run_error(e: HostError) -> String {
    match e {
        HostError::Run(why) => why,
        HostError::Setup(why) => panic!("a setup error, not a run one: {why}"),
    }
}

#[test]
fn a_config_is_read_or_refused_naming_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let (path, cfg) = config_in(dir.path());
    assert_eq!(read_config(&path), Ok(cfg));
    let missing = dir.path().join("none.toml");
    let why = setup_error(read_config(&missing).unwrap_err());
    assert!(
        why.starts_with(&format!("{}: ", missing.display())),
        "{why}"
    );
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "zyxname = [").unwrap();
    let why = setup_error(read_config(&bad).unwrap_err());
    assert!(why.starts_with(&format!("{}: ", bad.display())), "{why}");
}

#[test]
fn a_device_loads_only_with_a_writable_state_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (_, cfg) = config_in(dir.path());
    let device = load_device(cfg.clone()).unwrap();
    assert_eq!(device.cfg, cfg);
    assert!(!cfg.state_file.exists(), "nothing written but checked");
    // A folder that is not there: found at the start.
    let gone = DeviceConfig {
        state_file: dir.path().join("none/state.toml"),
        ..cfg.clone()
    };
    let why = setup_error(load_device(gone).unwrap_err());
    assert!(why.starts_with("the state file cannot be written"), "{why}");
    // A state file that does not hold.
    std::fs::write(&cfg.state_file, "zyxname = [").unwrap();
    setup_error(load_device(cfg).unwrap_err());
}

#[test]
fn the_box_s_interfaces_include_the_loopback_address() {
    let all = interfaces().unwrap();
    assert!(
        all.iter()
            .any(|i| i.ip == Ipv4Addr::LOCALHOST && i.netmask == Ipv4Addr::new(255, 0, 0, 0)),
        "{all:?}"
    );
}

#[test]
fn an_interface_without_a_usable_mac_is_setup_and_one_not_there_a_run_error() {
    let lo = IfAddr {
        name: "lo".into(),
        ip: Ipv4Addr::LOCALHOST,
        netmask: Ipv4Addr::new(255, 0, 0, 0),
    };
    // The loopback's MAC is all zeros: no hardware address.
    let why = setup_error(interface_info(&lo).unwrap_err());
    assert_eq!(why, "the interface has no usable MAC address");
    let none = IfAddr {
        name: "zyxnone0".into(),
        ..lo
    };
    let why = run_error(interface_info(&none).unwrap_err());
    assert!(why.starts_with("the interface's MAC address: "), "{why}");
}

#[test]
fn prepare_refuses_as_the_binary_did_before_anything_is_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = config_in(dir.path());
    let opts = |config: PathBuf, place: Place| options(config, place, dir.path());
    // The config: exit 2.
    let missing = dir.path().join("none.toml");
    let e = prepare(&opts(missing, Place::Ip(NOWHERE)), &[])
        .err()
        .unwrap();
    assert_eq!(exit_code(&e), 2, "{e}");
    // An engine channel the device lacks (16 TX channels): exit 2, before
    // the interface is looked at.
    for engine in [&[16][..], &[3, 20]] {
        let e = prepare(&opts(path.clone(), Place::Ip(NOWHERE)), engine)
            .err()
            .unwrap();
        let why = setup_error(e);
        assert_eq!(
            why,
            format!(
                "the engine's TX channel {} is past the device's 16 TX channels",
                engine.last().unwrap() + 1
            )
        );
    }
    // An address no interface holds (yet), an interface not there: exit 1,
    // which the unit retries; the engine's channels were the device's.
    for place in [Place::Ip(NOWHERE), Place::Iface("zyxnone0".into())] {
        let e = prepare(&opts(path.clone(), place.clone()), &[15])
            .err()
            .unwrap();
        let why = run_error(e);
        assert!(!why.contains("192.0.2"), "no address in {why}");
    }
    assert!(!dir.path().join("control.sock").exists(), "nothing bound");
}

// ---- the running device ----

/// A host whose device thread runs `body` (the real thread runs serving
/// and the goodbyes).
fn hosting(body: impl FnOnce(&Control) -> Result<(), HostError> + Send + 'static) -> Host {
    let control = Arc::new(Control::default());
    let theirs = Arc::clone(&control);
    let thread = std::thread::spawn(move || body(&theirs));
    Host { control, thread }
}

/// Waits (bounded) until the host asks serving to end, then takes a little
/// while to end, as the stops and the goodbyes do.
fn asked_then_ends(control: &Control) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async { tokio::time::timeout(BOUND, control.stop.notified()).await })
        .expect("the stop is asked within the bound");
    std::thread::sleep(Duration::from_millis(20));
}

#[test]
fn a_stop_asks_serving_to_end_and_waits_for_the_device() {
    let host = hosting(|c| {
        asked_then_ends(c);
        Ok(())
    });
    assert_eq!(host.ended(), None, "serving");
    assert_eq!(host.stop(BOUND), Stopped::Done);
}

#[test]
fn a_device_that_failed_on_its_way_out_is_failed_with_its_error() {
    let host = hosting(|c| {
        asked_then_ends(c);
        Err(HostError::Run("the mDNS shutdown was not confirmed".into()))
    });
    assert_eq!(
        host.stop(BOUND),
        Stopped::Failed("the mDNS shutdown was not confirmed".into())
    );
    let host = hosting(|c| {
        asked_then_ends(c);
        panic!("zyxboom");
    });
    assert_eq!(host.stop(BOUND), Stopped::Failed(PANICKED.into()));
}

#[test]
fn a_device_still_running_at_the_end_of_the_wait_is_left_to_end() {
    let (release, released) = mpsc::channel::<()>();
    let host = hosting(move |_| {
        // Deaf to the stop: ends when the test releases it (or at the
        // bound).
        let _ = released.recv_timeout(BOUND);
        Ok(())
    });
    assert_eq!(host.ended(), None);
    assert_eq!(host.stop(Duration::from_millis(50)), Stopped::StillRunning);
    release.send(()).unwrap();
}

#[test]
fn serving_that_ended_by_itself_is_seen_before_the_stop() {
    let (ended, seen) = mpsc::channel::<()>();
    let host = hosting(move |c| {
        let served = settle(c, Err(ServeError::Control(io::Error::other("zyxgone"))));
        ended.send(()).unwrap();
        // The RX stops and the goodbyes still run before the thread ends.
        asked_then_ends(c);
        served
    });
    seen.recv_timeout(BOUND).expect("serving ends");
    let why = "the control socket stopped: zyxgone";
    assert_eq!(host.ended().as_deref(), Some(why));
    assert_eq!(host.stop(BOUND), Stopped::Failed(why.into()));
}

#[test]
fn wait_is_the_device_thread_s_own_end() {
    assert_eq!(hosting(|_| Ok(())).wait(), Ok(()));
    assert_eq!(
        hosting(|_| Err(HostError::Setup("zyxwhy".into()))).wait(),
        Err(HostError::Setup("zyxwhy".into()))
    );
    assert_eq!(
        hosting(|_| panic!("zyxboom")).wait(),
        Err(HostError::Run(PANICKED.into()))
    );
}
