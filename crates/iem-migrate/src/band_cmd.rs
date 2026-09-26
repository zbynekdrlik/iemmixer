//! `iem-migrate band` (S4 design note §3.4, #20 design note §7): the
//! predecessor's data directory → the server's band directory. Everything is
//! checked first; nothing is written unless every category maps (and never
//! with `--dry-run`). The output is transactional: it is written into a
//! staging copy of the band directory and swapped in whole ([`crate::stage`]).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use iem_core::band::{CustomizationFile, Preset, PresetFile, Snapshot, SnapshotFile};
use iem_core::{Customization, MixSnapshot, PresetEntry};
use iem_engine_proto::MixId;
use iem_rpp::aliases::{Aliases, Eras, MemberAlias, parse_aliases, parse_eras};
use iem_rpp::band::{Ctx, Stats, rekey_customization, rekey_presets, rekey_snapshots};
use iem_rpp::topology::Topology;
use iem_server::band_import::{
    DefaultPins, FileOutcome, LegacyConfig, PinOutcome, PinRequest, check_jwt_secret, check_vapid,
    import_photo, import_pins, import_push, import_secret, import_tls, parse_default_pins,
    parse_legacy_config,
};
use iem_server::pin_hash::is_valid_pin_format;
use iem_server::pin_store::ENGINEER_ID;
use iem_server::secrets::{JWT_SECRET_FILE, SECRETS_DIR, VAPID_PRIVATE_FILE};
use serde::de::DeserializeOwned;

use crate::args::parse;
use crate::stage::{MARKER, Stage, Step, no_faults, recover, siblings};
use crate::{Failure, read_text, site};

/// The predecessor's server config file in its data directory.
pub const LEGACY_CONFIG: &str = "config.yaml";
pub const LEGACY_PINS: &str = "pins.json";

struct Plan<'a> {
    legacy: &'a Path,
    out: &'a Path,
    partial: bool,
    aliases: &'a Aliases,
    eras: &'a Eras,
    topology: &'a Topology,
    report: Vec<String>,
    problems: Vec<String>,
    /// JSON files to write (path inside the band directory, text).
    files: Vec<(PathBuf, String)>,
    /// Photos to copy (source, path inside the band directory).
    photos: Vec<(PathBuf, PathBuf)>,
    pins: Vec<PinRequest>,
    config: Option<LegacyConfig>,
}

fn ctx<'b>(p: &Plan<'b>, legacy_member: &'b str, member: &'b MemberAlias) -> Ctx<'b> {
    Ctx {
        topology: p.topology,
        aliases: p.aliases,
        eras: p.eras,
        legacy_member,
        member,
    }
}

impl<'a> Plan<'a> {
    fn missing(&mut self, what: &str) {
        if self.partial {
            self.report.push(format!("absent: {what}"));
        } else {
            self.problems.push(format!(
                "{what} is missing (--partial accepts a partial copy)"
            ));
        }
    }

    fn member(&mut self, file: &str, legacy_id: &str) -> Option<&'a MemberAlias> {
        let aliases: &'a Aliases = self.aliases;
        let m = aliases.members.get(legacy_id);
        if m.is_none() {
            self.problems.push(format!(
                "{file}: member {legacy_id:?} is not in the aliases"
            ));
        }
        m
    }

    fn check_members(&mut self) {
        let mut ids: BTreeMap<&str, &str> = BTreeMap::new();
        let aliases: &'a Aliases = self.aliases;
        let topology: &'a Topology = self.topology;
        for (legacy, m) in &aliases.members {
            if topology.mix(&MixId::new(m.mix.clone())).is_none() {
                self.problems.push(format!(
                    "member {legacy}: {} is not a mix of the site",
                    m.mix
                ));
            }
            if !m.archived
                && let Some(other) = ids.insert(&m.id, legacy)
            {
                self.problems.push(format!(
                    "members {other} and {legacy} are both iemmixer member {} (mark the renamed one archived)",
                    m.id
                ));
            }
        }
    }

    /// `<legacy>/<sub>/*.<ext>` as (file stem, path), sorted; `None` when the
    /// directory does not exist.
    fn list(&mut self, sub: &str, ext: &str) -> Option<Vec<(String, PathBuf)>> {
        let dir = self.legacy.join(sub);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                self.problems.push(format!("{}: {e}", dir.display()));
                return Some(Vec::new());
            }
        };
        let mut out: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
            .filter_map(|p| Some((p.file_stem()?.to_str()?.to_owned(), p)))
            .collect();
        out.sort();
        Some(out)
    }

    fn read_json<T: DeserializeOwned>(&mut self, path: &Path) -> Option<T> {
        let parsed = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()));
        match parsed {
            Ok(v) => Some(v),
            Err(e) => {
                self.problems.push(format!("{}: {e}", path.display()));
                None
            }
        }
    }

    fn json_out<T: serde::Serialize>(&mut self, path: PathBuf, value: &T) {
        match serde_json::to_string_pretty(value) {
            Ok(text) => self.files.push((path, text)),
            Err(e) => self.problems.push(format!("{}: {e}", path.display())),
        }
    }

    fn stats(&mut self, what: &str, id: &str, n: usize, st: Stats) {
        self.report.push(format!(
            "{what} {id}: {n} (sends {}, input EQs kept {}, other mixes' EQs dropped {}, between eras {})",
            st.sends, st.input_eqs, st.dropped_mix_eqs, st.between_eras
        ));
    }

    fn presets(&mut self) {
        let Some(files) = self.list("presets", "json") else {
            self.report.push("presets: none".into());
            return;
        };
        let mut by_member: BTreeMap<String, (Vec<Preset>, Stats)> = BTreeMap::new();
        for (legacy, path) in files {
            let file = format!("presets/{legacy}.json");
            let Some(m) = self.member(&file, &legacy) else {
                continue;
            };
            let Some(entries) = self.read_json::<HashMap<String, PresetEntry>>(&path) else {
                continue;
            };
            match rekey_presets(&entries, &ctx(self, &legacy, m)) {
                Ok((list, st)) => {
                    let slot = by_member.entry(m.id.clone()).or_default();
                    slot.0.extend(list);
                    slot.1 += st;
                }
                Err(p) => self
                    .problems
                    .extend(p.0.into_iter().map(|x| format!("{file}: {x}"))),
            }
        }
        for (id, (list, st)) in by_member {
            self.stats("presets", &id, list.len(), st);
            let path = Path::new("presets").join(format!("{id}.json"));
            self.json_out(path, &PresetFile::new(id, list));
        }
    }

    fn snapshots(&mut self) {
        let Some(files) = self.list("snapshots", "json") else {
            self.report.push("snapshots: none".into());
            return;
        };
        let mut by_member: BTreeMap<String, (Vec<Snapshot>, Stats)> = BTreeMap::new();
        for (legacy, path) in files {
            let file = format!("snapshots/{legacy}.json");
            let Some(m) = self.member(&file, &legacy) else {
                continue;
            };
            let Some(list) = self.read_json::<Vec<MixSnapshot>>(&path) else {
                continue;
            };
            match rekey_snapshots(&list, &ctx(self, &legacy, m)) {
                Ok((list, st)) => {
                    let slot = by_member.entry(m.id.clone()).or_default();
                    slot.0.extend(list);
                    slot.1 += st;
                }
                Err(p) => self
                    .problems
                    .extend(p.0.into_iter().map(|x| format!("{file}: {x}"))),
            }
        }
        for (id, (list, st)) in by_member {
            self.stats("snapshots", &id, list.len(), st);
            let path = Path::new("snapshots").join(format!("{id}.json"));
            self.json_out(path, &SnapshotFile::new(id, list));
        }
    }

    fn customizations(&mut self) {
        let Some(files) = self.list("customizations", "json") else {
            self.report.push("customizations: none".into());
            return;
        };
        for (legacy, path) in files {
            let file = format!("customizations/{legacy}.json");
            let Some(m) = self.member(&file, &legacy) else {
                continue;
            };
            if m.archived {
                self.report
                    .push(format!("customizations {legacy}: ignored (renamed member)"));
                continue;
            }
            let Some(c) = self.read_json::<Customization>(&path) else {
                continue;
            };
            match rekey_customization(&c, &ctx(self, &legacy, m)) {
                Ok((pinned, hidden)) => {
                    self.report.push(format!(
                        "customizations {}: {} pinned, {} hidden",
                        m.id,
                        pinned.len(),
                        hidden.len()
                    ));
                    let path = Path::new("customizations").join(format!("{}.json", m.id));
                    self.json_out(path, &CustomizationFile::new(m.id.clone(), pinned, hidden));
                }
                Err(p) => self
                    .problems
                    .extend(p.0.into_iter().map(|x| format!("{file}: {x}"))),
            }
        }
    }

    fn photos(&mut self) {
        let Some(files) = self.list("photos", "jpg") else {
            self.report.push("photos: none".into());
            return;
        };
        for (legacy, path) in files {
            let Some(m) = self.member(&format!("photos/{legacy}.jpg"), &legacy) else {
                continue;
            };
            if m.archived {
                self.report
                    .push(format!("photo {legacy}: ignored (renamed member)"));
                continue;
            }
            let rel = Path::new("photos").join(format!("{}.jpg", m.id));
            match import_photo(&path, &self.out.join(&rel), true) {
                Ok(()) => {
                    self.report.push(format!("photo {}", m.id));
                    self.photos.push((path, rel));
                }
                Err(e) => self.problems.push(e.to_string()),
            }
        }
    }

    fn config(&mut self) {
        let path = self.legacy.join(LEGACY_CONFIG);
        match std::fs::read_to_string(&path) {
            Ok(t) => match parse_legacy_config(&t) {
                Ok(c) => self.config = Some(c),
                Err(e) => self.problems.push(format!("{}: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.missing(LEGACY_CONFIG),
            Err(e) => self.problems.push(format!("{}: {e}", path.display())),
        }
    }

    fn pins(&mut self, defaults: &DefaultPins) {
        let path = self.legacy.join(LEGACY_PINS);
        let legacy_pins: HashMap<String, String> = if path.exists() {
            self.read_json(&path).unwrap_or_default()
        } else {
            self.report.push(format!(
                "absent: {LEGACY_PINS} (every member on the default PIN)"
            ));
            HashMap::new()
        };
        let aliases: &'a Aliases = self.aliases;
        let mut ignored = Vec::new();
        for legacy in legacy_pins.keys() {
            match aliases.members.get(legacy) {
                None => self.problems.push(format!(
                    "{LEGACY_PINS}: member {legacy:?} is not in the aliases"
                )),
                Some(m) if m.archived || m.id == ENGINEER_ID => ignored.push(legacy.clone()),
                Some(_) => {}
            }
        }
        ignored.sort();
        if !ignored.is_empty() {
            self.report.push(format!(
                "{LEGACY_PINS}: {} entr(ies) ignored (renamed members, or the engineer, whose PIN is in the config)",
                ignored.len()
            ));
        }
        let (mut own, mut default) = (0, 0);
        for (legacy, m) in &aliases.members {
            if m.archived || m.id == ENGINEER_ID {
                continue;
            }
            let pin = match (legacy_pins.get(legacy), &defaults.member) {
                (Some(p), _) => {
                    own += 1;
                    p.clone()
                }
                (None, Some(d)) => {
                    default += 1;
                    d.clone()
                }
                (None, None) => {
                    self.problems.push(format!(
                        "member {}: no PIN in {LEGACY_PINS} and no default (--legacy-default-pins member=…)",
                        m.id
                    ));
                    continue;
                }
            };
            if !is_valid_pin_format(&pin) {
                self.problems.push(format!(
                    "member {}: the predecessor's PIN is not 4 digits",
                    m.id
                ));
                continue;
            }
            self.pins.push(PinRequest {
                owner: m.id.clone(),
                pin,
            });
        }
        let engineer = self
            .config
            .as_ref()
            .and_then(|c| c.engineer_pin.clone())
            .map(|p| (p, "the predecessor's config"))
            .or_else(|| {
                defaults
                    .engineer
                    .clone()
                    .map(|p| (p, "the predecessor's default"))
            });
        match engineer {
            Some((pin, from)) if is_valid_pin_format(&pin) => {
                self.report.push(format!("engineer PIN from {from}"));
                self.pins.push(PinRequest {
                    owner: ENGINEER_ID.into(),
                    pin,
                });
            }
            Some(_) => self
                .problems
                .push("the engineer PIN is not 4 digits".into()),
            None => self.missing(
                "the engineer PIN (config engineer_pin or --legacy-default-pins engineer=…)",
            ),
        }
        self.report.push(format!(
            "pins: {own} member(s) with their own PIN, {default} on the predecessor's default PIN"
        ));
    }

    fn secrets(&mut self) {
        let Some(c) = self.config.clone() else { return };
        let dir = self.out.join(SECRETS_DIR);
        self.secret(
            "jwt_secret",
            c.jwt_secret.as_deref(),
            check_jwt_secret,
            &dir.join(JWT_SECRET_FILE),
        );
        self.secret(
            "vapid_private_key",
            c.vapid_private_key.as_deref(),
            check_vapid,
            &dir.join(VAPID_PRIVATE_FILE),
        );
        match import_tls(
            &self.legacy.join(&c.tls_cert),
            &self.legacy.join(&c.tls_key),
            self.out,
            true,
        ) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.missing("the LAN certificate (tls_cert / tls_key)");
            }
            Err(e) => self.problems.push(e.to_string()),
        }
        if let Err(e) = import_push(self.legacy, self.out, true) {
            self.problems.push(e.to_string());
        }
    }

    fn secret(
        &mut self,
        name: &str,
        value: Option<&str>,
        check: fn(&str) -> Result<(), String>,
        path: &Path,
    ) {
        let Some(v) = value else {
            self.missing(&format!("{LEGACY_CONFIG} {name}"));
            return;
        };
        let checked = check(v).and_then(|()| {
            import_secret(path, v, true)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        if let Err(e) = checked {
            self.problems.push(e);
        }
    }

    fn backups(&mut self) {
        let Some(files) = self.list("backups", "json") else {
            self.report.push("backups: none".into());
            return;
        };
        let n = files.len();
        for (name, path) in files {
            let Some(mut v) = self.read_json::<serde_json::Value>(&path) else {
                continue;
            };
            if let Some(obj) = v.as_object_mut() {
                obj.remove("pins");
            }
            let dst = Path::new("legacy")
                .join("backups")
                .join(format!("{name}.json"));
            self.json_out(dst, &v);
        }
        self.report.push(format!(
            "backups: {n} archived without PINs under legacy/backups"
        ));
    }

    /// Writes every output into the staging directory; returns the PIN
    /// outcomes. Each item is a fault-injection point.
    fn write(
        &mut self,
        stage: &mut Stage,
        fail: &dyn Fn(Step) -> io::Result<()>,
    ) -> Result<Vec<(String, PinOutcome)>, Failure> {
        let dir = stage.dir().to_path_buf();
        let io = |p: &Path, e: io::Error| Failure::io(format!("{}: {e}", p.display()));
        for (rel, text) in &self.files {
            stage.step(fail).map_err(|e| io(rel, e))?;
            stage
                .write(rel, text.as_bytes())
                .map_err(|e| io(&self.out.join(rel), e))?;
        }
        for (src, rel) in &self.photos {
            stage.step(fail).map_err(|e| io(rel, e))?;
            import_photo(src, &dir.join(rel), false).map_err(|e| io(src, e))?;
        }
        let secrets = dir.join(SECRETS_DIR);
        stage.step(fail).map_err(|e| io(&secrets, e))?;
        let outcomes = import_pins(&secrets, &self.pins, false)
            .map_err(|e| Failure::io(format!("{}: {e}", self.out.join(SECRETS_DIR).display())))?;
        if let Some(c) = &self.config {
            for (value, file) in [
                (&c.jwt_secret, JWT_SECRET_FILE),
                (&c.vapid_private_key, VAPID_PRIVATE_FILE),
            ] {
                if let Some(v) = value {
                    let path = secrets.join(file);
                    stage.step(fail).map_err(|e| io(&path, e))?;
                    let outcome = import_secret(&path, v, false).map_err(|e| io(&path, e))?;
                    self.report.push(format!(
                        "{file}: {}",
                        if outcome == FileOutcome::Created {
                            "imported"
                        } else {
                            "unchanged"
                        }
                    ));
                }
            }
            let (cert, key) = (self.legacy.join(&c.tls_cert), self.legacy.join(&c.tls_key));
            if cert.exists() && key.exists() {
                stage.step(fail).map_err(|e| io(&cert, e))?;
                import_tls(&cert, &key, &dir, false).map_err(|e| io(&cert, e))?;
                self.report.push("LAN certificate imported".into());
            }
            stage.step(fail).map_err(|e| io(self.legacy, e))?;
            let (added, total) =
                import_push(self.legacy, &dir, false).map_err(|e| io(self.legacy, e))?;
            self.report.push(format!(
                "push subscriptions: {added} added, {total} in total"
            ));
        }
        Ok(outcomes)
    }

    fn pin_report(&mut self, outcomes: &[(String, PinOutcome)]) {
        for (owner, o) in outcomes {
            let what = match o {
                PinOutcome::Set => "set",
                PinOutcome::Unchanged => "unchanged",
                PinOutcome::KeptIemmixerPin => "kept (set in iemmixer)",
            };
            self.report.push(format!("pin {owner}: {what}"));
        }
    }
}

pub fn run(args: &[String]) -> Result<String, Failure> {
    run_with(args, &no_faults)
}

/// [`run`] with a fault-injection hook at every write step (tests).
pub fn run_with(args: &[String], fail: &dyn Fn(Step) -> io::Result<()>) -> Result<String, Failure> {
    let a = parse(
        args,
        &[
            "--legacy",
            "--aliases",
            "--eras",
            "--site",
            "--out",
            "--legacy-default-pins",
        ],
        &["--partial", "--dry-run"],
    )?;
    let legacy = a.path("--legacy")?;
    let out = a.path("--out")?;
    let dry = a.flag("--dry-run");
    if !legacy.is_dir() {
        return Err(Failure::input(format!(
            "{}: not a directory",
            legacy.display()
        )));
    }
    let aliases = parse_aliases(&read_text(&a.path("--aliases")?)?).map_err(Failure::input)?;
    let eras = parse_eras(&read_text(&a.path("--eras")?)?).map_err(Failure::input)?;
    let site = site::open(&a.path("--site")?)?;
    let defaults = match a.opt_path("--legacy-default-pins") {
        Some(p) => parse_default_pins(&read_text(&p)?).map_err(Failure::input)?,
        None => DefaultPins::default(),
    };
    let mut plan = Plan {
        legacy: &legacy,
        out: &out,
        partial: a.flag("--partial"),
        aliases: &aliases,
        eras: &eras,
        topology: &site.topology,
        report: vec![format!("band data from {}", legacy.display())],
        problems: Vec::new(),
        files: Vec::new(),
        photos: Vec::new(),
        pins: Vec::new(),
        config: None,
    };
    plan.check_members();
    plan.config();
    plan.presets();
    plan.snapshots();
    plan.customizations();
    plan.photos();
    plan.pins(&defaults);
    plan.secrets();
    plan.backups();
    if !plan.problems.is_empty() {
        return Err(Failure::input(format!(
            "{} problem(s), nothing written:\n  - {}",
            plan.problems.len(),
            plan.problems.join("\n  - ")
        )));
    }
    let io = |e: io::Error| Failure::io(format!("{}: {e}", out.display()));
    if dry {
        let (staging, old) = siblings(&out).map_err(io)?;
        for p in [staging, old] {
            if p.exists() {
                plan.report.push(format!(
                    "note: an interrupted run left {} (complete: {}); a real run recovers it first",
                    p.display(),
                    p.join(MARKER).exists()
                ));
            }
        }
        let secrets = out.join(SECRETS_DIR);
        let outcomes = import_pins(&secrets, &plan.pins, true)
            .map_err(|e| Failure::io(format!("{}: {e}", secrets.display())))?;
        plan.pin_report(&outcomes);
        plan.report.push(format!(
            "dry run: nothing written ({} file(s) and {} photo(s) would be)",
            plan.files.len(),
            plan.photos.len()
        ));
        return Ok(plan.report.join("\n"));
    }
    let recovered = recover(&out).map_err(io)?;
    plan.report.extend(recovered);
    let mut stage = Stage::begin(&out, fail).map_err(io)?;
    let outcomes = match plan.write(&mut stage, fail) {
        Ok(o) => o,
        Err(e) => {
            stage.abort();
            return Err(e);
        }
    };
    let notes = stage.commit(fail).map_err(io)?;
    plan.pin_report(&outcomes);
    plan.report.push(format!(
        "written to {}: {} file(s), {} photo(s)",
        out.display(),
        plan.files.len(),
        plan.photos.len()
    ));
    plan.report.extend(notes);
    Ok(plan.report.join("\n"))
}
