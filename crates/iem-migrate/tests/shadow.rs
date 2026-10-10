//! `iem-migrate shadow` end to end (S8 lane 4, iemmixer#11): a report-only
//! import of a synthetic predecessor project on `config/test-site.toml`
//! (P6). It prints one JSON object and writes nothing: not the state
//! directory, not a missing one, not the project.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use iem_engine::core::{reconcile, to_state};
use iem_engine::persist::{Persisted, Store, encode};
use iem_migrate::{EXIT_INPUT, EXIT_IO, run, site};
use iem_rpp::aliases::MemberAlias;
use iem_rpp::sitegen::{aliases_toml, project, sample_state, synthetic_routing, track_name};
use serde_json::Value;

fn site_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}

/// A synthetic project of `seed`'s state and its aliases in a new directory.
struct World {
    dir: tempfile::TempDir,
    rpp: PathBuf,
    aliases: PathBuf,
}

impl World {
    fn new(seed: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let t = site::open(&site_path()).unwrap().topology;
        let r = synthetic_routing(&t);
        let rpp = dir.path().join("project.RPP");
        let state = sample_state(&t, &r, seed);
        std::fs::write(&rpp, project(&t, &r, &state, &track_name).unwrap()).unwrap();
        let member = |id: &str, archived: bool| MemberAlias {
            id: id.into(),
            mix: id.into(),
            archived,
        };
        let members = BTreeMap::from([
            ("m1".to_owned(), member("member1", false)),
            ("m2".to_owned(), member("member2", false)),
            ("old1".to_owned(), member("member1", true)),
            ("eng".to_owned(), member("engineer", false)),
        ]);
        let aliases = dir.path().join("aliases.toml");
        std::fs::write(&aliases, aliases_toml(&t, &r, &members)).unwrap();
        Self { dir, rpp, aliases }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn args(&self, cmd: &str, state: &Path) -> Vec<String> {
        vec![
            cmd.to_owned(),
            "--rpp".into(),
            s(&self.rpp),
            "--aliases".into(),
            s(&self.aliases),
            "--site".into(),
            s(&site_path()),
            "--state-dir".into(),
            s(state),
        ]
    }

    /// The import's own state directory.
    fn imported(&self) -> PathBuf {
        let dir = self.path("state");
        run(&self.args("import", &dir)).unwrap();
        dir
    }

    fn shadow(&self, state: &Path) -> Value {
        let out = run(&self.args("shadow", state)).unwrap();
        assert!(!out.contains('\n'), "one line: {out}");
        serde_json::from_str(&out).unwrap()
    }
}

/// Every file under `dir` (relative path → bytes); none when it is missing.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy();
                out.insert(rel.replace('\\', "/"), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}

#[test]
fn the_shadow_of_the_imported_project_is_clean_and_writes_nothing() {
    let w = World::new(1);
    let dir = w.imported();
    let (state, project) = (tree(&dir), std::fs::read(&w.rpp).unwrap());
    // A running engine's lock never stops it: the shadow takes no lock.
    let store = Store::open(&dir).unwrap();
    let lock = store.lock().unwrap();
    let r = w.shadow(&dir);
    drop(lock);
    assert_eq!(
        r,
        serde_json::json!({
            "import": "writes",
            "counts": {"tracks": 45, "sends": 268, "eqs": 44, "limiters": 10, "trims": 24},
            "site": [],
            "fit": 0,
            "state_from": "current",
            "state": [],
            "doubts": 0,
        })
    );
    assert_eq!(tree(&dir), state, "the state directory is untouched");
    assert_eq!(std::fs::read(&w.rpp).unwrap(), project);
}

#[test]
fn a_missing_state_directory_is_compared_with_nothing_and_never_created() {
    let w = World::new(2);
    let dir = w.path("no-state");
    let r = w.shadow(&dir);
    assert_eq!(r["import"], "writes");
    assert_eq!(r["state_from"], "none");
    assert_eq!(r["state"], serde_json::json!([]));
    assert!(!dir.exists(), "the shadow created the state directory");
}

#[test]
fn the_band_s_changes_are_named_by_id_and_field_never_by_value() {
    let w = World::new(3);
    let dir = w.imported();
    let sf = site::open(&site_path()).unwrap();
    let edited = to_state(
        &sf.compiled,
        &reconcile(
            &sf.compiled,
            &sample_state(&sf.topology, &synthetic_routing(&sf.topology), 77),
        )
        .0,
    );
    Store::open(&dir)
        .unwrap()
        .save(&Persisted {
            topology_hash: sf.compiled.hash.clone(),
            state: edited,
            ..Persisted::default()
        })
        .unwrap();
    let before = tree(&dir);
    let r = w.shadow(&dir);
    assert_eq!(r["import"], "writes");
    let diffs = r["state"].as_array().unwrap();
    assert!(!diffs.is_empty(), "{r}");
    // Kind, id and field only: no value has a place to go.
    let kinds = ["input", "mix", "level", "mix_level", "group"];
    for d in diffs {
        let o = d.as_object().unwrap();
        assert_eq!(o.len(), 3, "{d}");
        assert!(kinds.contains(&o["kind"].as_str().unwrap()), "{d}");
        let field = o["field"].as_str().unwrap();
        assert!(field.starts_with(|c: char| c.is_ascii_lowercase()), "{d}");
        assert!(o["id"].as_str().is_some(), "{d}");
    }
    assert_eq!(tree(&dir), before, "the state directory is untouched");
}

#[test]
fn an_interrupted_save_is_read_where_it_is_never_recovered() {
    let w = World::new(4);
    let dir = w.imported();
    let newer = encode(&Persisted {
        rev: 9,
        ..Persisted::default()
    })
    .unwrap();
    std::fs::write(dir.join("save.tmp"), newer).unwrap();
    let before = tree(&dir);
    let r = w.shadow(&dir);
    assert_eq!(r["state_from"], "interrupted", "{r}");
    assert!(!r["state"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(tree(&dir), before, "the interrupted save was touched");
}

#[test]
fn a_project_that_does_not_import_is_reported_by_its_problem_count() {
    let w = World::new(5);
    let text = std::fs::read_to_string(&w.rpp).unwrap();
    let renamed = text.replacen(&track_name("mic2"), "zyxqwunknown trk", 1);
    assert_ne!(renamed, text);
    std::fs::write(&w.rpp, renamed).unwrap();
    let r = w.shadow(&w.path("state"));
    assert_eq!(r["import"], "unmappable", "{r}");
    assert!(r["problems"].as_u64().unwrap() >= 1, "{r}");
    assert_eq!(r.as_object().unwrap().len(), 2, "no track name: {r}");
}

#[test]
fn bad_inputs_fail_and_name_why() {
    let w = World::new(6);
    let dir = w.path("state");
    // Every option is required.
    let mut a = w.args("shadow", &dir);
    a.truncate(7);
    assert_eq!(run(&a).unwrap_err().code, EXIT_INPUT);
    // A site file without an [engine] table has nothing to compare with.
    let bare = w.path("bare-site.toml");
    std::fs::write(&bare, "port = 1\n").unwrap();
    let mut a = w.args("shadow", &dir);
    a[6] = s(&bare);
    let e = run(&a).unwrap_err();
    assert_eq!(e.code, EXIT_INPUT);
    assert!(e.msg.contains("no [engine] table"), "{}", e.msg);
    // A project that cannot be read.
    let mut a = w.args("shadow", &dir);
    a[2] = s(&w.path("missing.RPP"));
    assert_eq!(run(&a).unwrap_err().code, EXIT_IO);
}

#[test]
fn the_binary_prints_the_report_on_stdout() {
    let w = World::new(7);
    let dir = w.imported();
    let a = w.args("shadow", &dir);
    let out = Command::new(env!("CARGO_BIN_EXE_iem-migrate"))
        .args(&a)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["import"], "writes");
    let help = Command::new(env!("CARGO_BIN_EXE_iem-migrate"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("iem-migrate shadow"));
}
