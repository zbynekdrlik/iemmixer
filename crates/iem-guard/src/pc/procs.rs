//! The process list and what it says (design §5.1, §5.2): the images, the
//! facts of a plan, the driver module's foreign holders, a foreign engine
//! and the adoption of a previous guard's children.

use super::Kid;
use crate::plan::Facts;
use crate::state::{Child, Children};

impl Children {
    /// The record of `kid`, if the guard started or adopted one.
    pub fn of(&self, kid: Kid) -> Option<&Child> {
        match kid {
            Kid::Engine => self.engine.as_ref(),
            Kid::Server => self.server.as_ref(),
            Kid::Tray => self.tray.as_ref(),
            Kid::Runner => self.runner.as_ref(),
        }
    }
}

/// The image names the process list is read for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Images {
    pub reaper: String,
    pub app: String,
    pub engine: String,
    pub server: String,
    pub tray: String,
    pub runner: String,
}

/// The once-a-second look (P10): the pids of each image, and the children of
/// ours that ended since the last look.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Procs {
    pub reaper: Vec<u32>,
    pub app: Vec<u32>,
    pub engine: Vec<u32>,
    pub server: Vec<u32>,
    pub tray: Vec<u32>,
    pub runner: Vec<u32>,
    /// Our children that ended, with their exit codes (`None`: no code could
    /// be read); the daemon applies `crash::after_exit` to the engine's.
    pub exited: Vec<(Kid, Option<i32>)>,
}

impl Procs {
    /// Picks the images out of a process list (names compare without ASCII
    /// case, as Windows does).
    pub fn from_list(list: &[(u32, String)], images: &Images) -> Self {
        let pick = |image: &str| -> Vec<u32> {
            list.iter()
                .filter(|(_, name)| name.eq_ignore_ascii_case(image))
                .map(|(pid, _)| *pid)
                .collect()
        };
        Self {
            reaper: pick(&images.reaper),
            app: pick(&images.app),
            engine: pick(&images.engine),
            server: pick(&images.server),
            tray: pick(&images.tray),
            runner: pick(&images.runner),
            exited: Vec::new(),
        }
    }

    /// REAPER or the predecessor app runs: the band's system is up (design
    /// §5.2, the reboot rule).
    pub fn band_up(&self) -> bool {
        !self.reaper.is_empty() || !self.app.is_empty()
    }

    /// The pids of one of our children's images.
    pub fn of(&self, kid: Kid) -> &[u32] {
        match kid {
            Kid::Engine => &self.engine,
            Kid::Server => &self.server,
            Kid::Tray => &self.tray,
            Kid::Runner => &self.runner,
        }
    }
}

/// The listening pids of ports 80 and 443 (`None`: free).
pub type Ports = (Option<u32>, Option<u32>);

/// The facts of a plan (design §5.1) from the process list, the driver
/// module's holders and the owners of ports 80/443.
///
/// An unreadable holder list assumes a running REAPER holds the card (the
/// handover checks it) and no foreign holder (`reaper_start` reads again and
/// refuses on one); unreadable ports assume a running app serves (the app
/// handover checks it). So a failed read never restarts a REAPER or an app
/// that serves the band.
pub fn facts_from(p: &Procs, holders: Option<&[(u32, String)]>, ports: Option<Ports>) -> Facts {
    let reaper = !p.reaper.is_empty();
    let app = !p.app.is_empty();
    let (reaper_holds_module, other_module_holder) = match holders {
        Some(h) => (
            h.iter().any(|(pid, _)| p.reaper.contains(pid)),
            h.iter()
                .any(|(pid, _)| !p.reaper.contains(pid) && !p.engine.contains(pid)),
        ),
        None => (reaper, false),
    };
    let app_serves = match ports {
        Some(ports) => app_serves(&p.app, ports),
        None => app,
    };
    Facts {
        reaper,
        app,
        engine: !p.engine.is_empty(),
        server: !p.server.is_empty(),
        tray: !p.tray.is_empty(),
        runner: !p.runner.is_empty(),
        reaper_holds_module,
        app_serves,
        other_module_holder,
    }
}

/// The predecessor app serves the band: its one process owns both ports 80
/// and 443. The plan's facts read it, and the app handover requires it
/// (#10: an iem-server that did not stop keeps the ports and answers the
/// app's HTTP checks itself).
pub fn app_serves(app: &[u32], (http, https): Ports) -> bool {
    match app {
        [pid] => http == Some(*pid) && https == Some(*pid),
        _ => false,
    }
}

/// The driver module's holders other than REAPER: they must leave before
/// REAPER starts (design §5.2 "back to event" step 4, I3).
pub fn foreign_holders(holders: &[(u32, String)], reaper: &[u32]) -> Vec<(u32, String)> {
    holders
        .iter()
        .filter(|(pid, _)| !reaper.contains(pid))
        .cloned()
        .collect()
}

/// Whether an engine runs that is not the guard's own child (`ours`).
pub fn foreign_engine(running: &[u32], ours: Option<u32>) -> bool {
    running.iter().any(|pid| Some(*pid) != ours)
}

/// Whether a running process is the child a previous guard started: the
/// same image path (without ASCII case) and the same start time, so a
/// recycled pid never passes for it (design §5.1, adoption).
pub fn adoptable(saved: &Child, image_path: &str, start_time: u64) -> bool {
    saved.image.eq_ignore_ascii_case(image_path) && saved.start_time == start_time
}
