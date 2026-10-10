//! The shadow import's comparison (S8 lane 4, iemmixer#11; design note
//! `docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md` §3.5),
//! pure: what an import of the saved project would write, against the
//! engine's saved state, and the project's topology against `site.toml`.
//! A difference is named by kind, id and field, never by value: the values
//! are the band's mix and the site's channels, and the report only counts
//! what moved. `shadow_cmd` reads the files and prints [`Report::json`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use iem_engine::core::{reconcile, to_state};
use iem_engine::persist::{Loaded, Source as Saved};
use iem_engine_proto::{Eq as EqSettings, Level, MixId, MixState};
use iem_rpp::import::{Imported, close, compare as capped, db_close, project};
use iem_rpp::topology::{Counts, Routing, Topology};
use serde_json::{Value, json};

use crate::import_cmd::CAP_TOLERANCE_DB;
use crate::site::SiteFile;

/// A value in the import's state only (a state field: `only_in_import`).
pub const ONLY_IMPORT: &str = "only_in_import";
/// A value in the engine's saved state only.
pub const ONLY_LIVE: &str = "only_in_live";
/// An id in the project's topology only (a topology field).
pub const ONLY_PROJECT: &str = "only_in_project";
/// An id in `site.toml` only.
pub const ONLY_SITE: &str = "only_in_site";

/// One difference: what (`kind`), whose (`id`; a mix's level, group strip
/// or heard mix is `<mix>/<id>`), which field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub kind: &'static str,
    pub id: String,
    pub field: String,
}

fn diff(kind: &'static str, id: &impl Display, field: &str) -> Diff {
    Diff {
        kind,
        id: id.to_string(),
        field: field.to_owned(),
    }
}

fn sorted<T: Ord + Clone>(v: &[T]) -> Vec<T> {
    let mut v = v.to_vec();
    v.sort();
    v
}

/// Ids on both sides by `id`: on one side only, `only_in_project` or
/// `only_in_site`; on both, the fields `both` names.
fn sides<'a, K: Ord + Display + 'a, T: 'a>(
    kind: &'static str,
    project: impl Iterator<Item = (&'a K, &'a T)>,
    site: impl Iterator<Item = (&'a K, &'a T)>,
    both: impl Fn(&T, &T) -> Vec<&'static str>,
    out: &mut Vec<Diff>,
) {
    let a: BTreeMap<&K, &T> = project.collect();
    let b: BTreeMap<&K, &T> = site.collect();
    for (id, x) in &a {
        match b.get(id) {
            None => out.push(diff(kind, id, ONLY_PROJECT)),
            Some(y) => out.extend(both(*x, *y).into_iter().map(|f| diff(kind, id, f))),
        }
    }
    for id in b.keys().filter(|id| !a.contains_key(*id)) {
        out.push(diff(kind, id, ONLY_SITE));
    }
}

/// Where the project's topology differs from `site`'s, as `Topology::diff`
/// finds it (the order of a group's inputs and of the heard mixes does not
/// matter): an input's `rx`, `talkback`; a group's `inputs`; a mix's `tx`,
/// `hears`; the engineer's `mix` (id: the project's, or `none`).
pub fn site_diffs(project: &Topology, site: &Topology) -> Vec<Diff> {
    let mut out = Vec::new();
    sides(
        "input",
        project.inputs.iter().map(|i| (&i.id, i)),
        site.inputs.iter().map(|i| (&i.id, i)),
        |x, y| {
            let mut f = Vec::new();
            if x.rx != y.rx {
                f.push("rx");
            }
            if x.talkback != y.talkback {
                f.push("talkback");
            }
            f
        },
        &mut out,
    );
    sides(
        "group",
        project.groups.iter().map(|g| (&g.id, g)),
        site.groups.iter().map(|g| (&g.id, g)),
        |x, y| {
            if sorted(&x.inputs) == sorted(&y.inputs) {
                Vec::new()
            } else {
                vec!["inputs"]
            }
        },
        &mut out,
    );
    sides(
        "mix",
        project.mixes.iter().map(|m| (&m.id, m)),
        site.mixes.iter().map(|m| (&m.id, m)),
        |x, y| {
            let mut f = Vec::new();
            if x.tx != y.tx {
                f.push("tx");
            }
            if sorted(&x.mixes) != sorted(&y.mixes) {
                f.push("hears");
            }
            f
        },
        &mut out,
    );
    if project.engineer != site.engineer {
        let id = project
            .engineer
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string);
        out.push(diff("engineer", &id, "mix"));
    }
    out
}

/// `iem_rpp::import::compare`'s comparison, its differences named instead
/// of printed with their values: dB within `tol`, other numbers up to
/// rounding, flags exactly.
struct Cmp {
    out: Vec<Diff>,
    tol: f64,
}

impl Cmp {
    fn push(&mut self, kind: &'static str, id: &str, field: &str) {
        self.out.push(diff(kind, &id, field));
    }

    fn db(&mut self, kind: &'static str, id: &str, field: &str, x: f64, y: f64) {
        if !db_close(x, y, self.tol) {
            self.push(kind, id, field);
        }
    }

    fn val(&mut self, kind: &'static str, id: &str, field: &str, x: f64, y: f64) {
        if !close(x, y) {
            self.push(kind, id, field);
        }
    }

    fn flag(&mut self, kind: &'static str, id: &str, field: &str, x: bool, y: bool) {
        if x != y {
            self.push(kind, id, field);
        }
    }

    /// `eq.gain`, and `eq.band<N>.kind|enabled|hz|gain|octaves` (N from 1).
    fn eq(&mut self, kind: &'static str, id: &str, x: &EqSettings, y: &EqSettings) {
        self.db(kind, id, "eq.gain", x.gain_db, y.gain_db);
        for (i, (p, q)) in x.bands.iter().zip(&y.bands).enumerate() {
            let band = |f: &str| format!("eq.band{}.{f}", i + 1);
            if p.kind != q.kind {
                self.push(kind, id, &band("kind"));
            }
            self.flag(kind, id, &band("enabled"), p.enabled, q.enabled);
            self.val(kind, id, &band("hz"), p.freq_hz, q.freq_hz);
            self.db(kind, id, &band("gain"), p.gain_db, q.gain_db);
            self.val(kind, id, &band("octaves"), p.bw_oct, q.bw_oct);
        }
    }

    fn levels<K: Ord + Display>(
        &mut self,
        kind: &'static str,
        mix: &MixId,
        a: &BTreeMap<K, Level>,
        b: &BTreeMap<K, Level>,
    ) {
        let ids: BTreeSet<&K> = a.keys().chain(b.keys()).collect();
        for k in ids {
            let id = format!("{mix}/{k}");
            match (a.get(k), b.get(k)) {
                (Some(x), Some(y)) => {
                    self.db(kind, &id, "gain", x.gain_db, y.gain_db);
                    self.val(kind, &id, "pan", x.pan, y.pan);
                    self.flag(kind, &id, "muted", x.muted, y.muted);
                }
                (Some(_), None) => self.push(kind, &id, ONLY_IMPORT),
                _ => self.push(kind, &id, ONLY_LIVE),
            }
        }
    }
}

/// Where `import` (the state an import would write) differs from `live`
/// (the engine's saved state) over what `topo`'s project can hold
/// (`routing`; `iem_rpp::import::project`). Kinds: `input` (`trim`,
/// `muted`, `processing`, `eq.*`), `mix` (`volume`, `muted`, `eq.*`,
/// `limiter`, `limit`), `level` (a mix's level of an input) and
/// `mix_level` (of a heard mix): `gain`, `pan`, `muted`; `group` (a mix's
/// group strip): `gain`, `muted`, `eq.*`. An id on one side only is
/// [`ONLY_IMPORT`] or [`ONLY_LIVE`].
pub fn state_diffs(
    topo: &Topology,
    routing: &Routing,
    import: &MixState,
    live: &MixState,
    tol: f64,
) -> Vec<Diff> {
    let (a, b) = (project(topo, routing, import), project(topo, routing, live));
    let mut c = Cmp {
        out: Vec::new(),
        tol,
    };
    let ids: BTreeSet<_> = a.inputs.keys().chain(b.inputs.keys()).collect();
    for id in ids {
        let who = id.to_string();
        match (a.inputs.get(id), b.inputs.get(id)) {
            (Some(x), Some(y)) => {
                c.db("input", &who, "trim", x.trim_db, y.trim_db);
                c.flag("input", &who, "muted", x.muted, y.muted);
                c.flag("input", &who, "processing", x.processing, y.processing);
                c.eq("input", &who, &x.eq, &y.eq);
            }
            (Some(_), None) => c.push("input", &who, ONLY_IMPORT),
            _ => c.push("input", &who, ONLY_LIVE),
        }
    }
    let ids: BTreeSet<_> = a.mixes.keys().chain(b.mixes.keys()).collect();
    for id in ids {
        let who = id.to_string();
        let (x, y) = match (a.mixes.get(id), b.mixes.get(id)) {
            (Some(x), Some(y)) => (x, y),
            (Some(_), None) => {
                c.push("mix", &who, ONLY_IMPORT);
                continue;
            }
            _ => {
                c.push("mix", &who, ONLY_LIVE);
                continue;
            }
        };
        c.db("mix", &who, "volume", x.out.volume_db, y.out.volume_db);
        c.flag("mix", &who, "muted", x.out.muted, y.out.muted);
        c.eq("mix", &who, &x.out.eq, &y.out.eq);
        let (p, q) = (&x.out.limiter, &y.out.limiter);
        c.flag("mix", &who, "limiter", p.enabled, q.enabled);
        c.db("mix", &who, "limit", p.limit_db, q.limit_db);
        c.levels("level", id, &x.inputs, &y.inputs);
        c.levels("mix_level", id, &x.mixes, &y.mixes);
        let groups: BTreeSet<_> = x.groups.keys().chain(y.groups.keys()).collect();
        for g in groups {
            let strip = format!("{id}/{g}");
            match (x.groups.get(g), y.groups.get(g)) {
                (Some(p), Some(q)) => {
                    c.db("group", &strip, "gain", p.gain_db, q.gain_db);
                    c.flag("group", &strip, "muted", p.muted, q.muted);
                    c.eq("group", &strip, &p.eq, &q.eq);
                }
                (Some(_), None) => c.push("group", &strip, ONLY_IMPORT),
                _ => c.push("group", &strip, ONLY_LIVE),
            }
        }
    }
    c.out
}

/// What an import would do, as `import` decides it: refuse on a topology
/// that differs from `site.toml`, then on values the engine would drop or
/// cap (`fit`), else write.
pub fn verdict(site: &[Diff], fit: usize) -> &'static str {
    if !site.is_empty() {
        "refuses_topology"
    } else if fit > 0 {
        "refuses_fit"
    } else {
        "writes"
    }
}

/// The saved state the engine would load, by the file it came from; none
/// when it would start on defaults (no state saved).
pub fn state_from(source: Saved) -> Option<&'static str> {
    match source {
        Saved::Current => Some("current"),
        Saved::Interrupted => Some("interrupted"),
        Saved::Generation(_) => Some("generation"),
        Saved::Baseline => Some("baseline"),
        Saved::Defaults => None,
    }
}

/// One shadow import's findings.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The project's REAPER counts (program spec §3.5).
    pub counts: Counts,
    /// The project's topology against `site.toml`.
    pub site: Vec<Diff>,
    /// Values of the project the engine would drop (not in `site.toml`) or
    /// cap: an import refuses them.
    pub fit: usize,
    /// The saved state compared with ([`state_from`]); `none`: nothing saved.
    pub state_from: &'static str,
    /// The import's state against that saved state.
    pub state: Vec<Diff>,
    /// Doubts the engine's load has about its saved state (`Loaded::doubts`).
    pub doubts: usize,
}

/// The shadow of `imp` (the import of the saved project) on `site`, against
/// `live` (the engine's saved state as it loads; none: no state directory).
/// The state an import would write is the import's own: reconciled on the
/// site and back (`iem-migrate import`).
pub fn shadow(imp: &Imported, site: &SiteFile, live: Option<&Loaded>) -> Report {
    let topology = site_diffs(&imp.topology, &site.topology);
    let (reconciled, dropped) = reconcile(&site.compiled, &imp.state);
    let would = to_state(&site.compiled, &reconciled);
    let moved = capped(
        &imp.topology,
        &imp.routing,
        &would,
        &imp.state,
        CAP_TOLERANCE_DB,
    );
    let saved = live.and_then(|l| state_from(l.source).map(|from| (from, l)));
    let state = saved.map_or_else(Vec::new, |(_, l)| {
        state_diffs(
            &imp.topology,
            &imp.routing,
            &would,
            &l.persisted.state,
            CAP_TOLERANCE_DB,
        )
    });
    Report {
        counts: imp.counts,
        site: topology,
        fit: dropped.len() + moved.len(),
        state_from: saved.map_or("none", |(from, _)| from),
        state,
        doubts: live.map_or(0, |l| l.doubts.len()),
    }
}

fn diffs_json(diffs: &[Diff]) -> Value {
    diffs
        .iter()
        .map(|d| json!({"kind": d.kind, "id": d.id, "field": d.field}))
        .collect()
}

impl Report {
    /// The report as one JSON object (`iem-migrate shadow`'s stdout).
    pub fn json(&self) -> Value {
        let c = &self.counts;
        json!({
            "import": verdict(&self.site, self.fit),
            "counts": {
                "tracks": c.tracks,
                "sends": c.sends,
                "eqs": c.eqs,
                "limiters": c.limiters,
                "trims": c.trims,
            },
            "site": diffs_json(&self.site),
            "fit": self.fit,
            "state_from": self.state_from,
            "state": diffs_json(&self.state),
            "doubts": self.doubts,
        })
    }
}

/// The report of a project that does not import: how many problems
/// (`iem_rpp::import::Problems`; their texts name tracks, so only the count).
pub fn unmappable(problems: usize) -> Value {
    json!({"import": "unmappable", "problems": problems})
}

#[cfg(test)]
mod tests;
