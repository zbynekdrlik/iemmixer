//! `iem-migrate` end to end on `config/test-site.toml` and synthetic
//! predecessor data (S4 design note §4). No site data (P6).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use iem_core::band::{CustomizationFile, PresetFile, SnapshotFile};
use iem_core::{ChannelPreset, ChannelSnapshot, Customization, MixSnapshot, PresetEntry};
use iem_engine::core::{reconcile, to_mix};
use iem_engine::persist::{Persisted, Source as Saved, Store};
use iem_engine_proto::{InputId, MixState, Source};
use iem_migrate::{EXIT_INPUT, EXIT_TOPOLOGY, run, site};
use iem_rpp::aliases::MemberAlias;
use iem_rpp::import::compare;
use iem_rpp::sitegen::{aliases_toml, project, sample_state, synthetic_site, track_name};
use iem_rpp::topology::Topology;
use iem_server::pepper;
use iem_server::pin_hash::PinHasher;
use iem_server::pin_store::PinStore;

fn site_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/test-site.toml")
}

fn topo() -> Topology {
    site::open(&site_path()).unwrap().topology
}

fn member(id: &str, archived: bool) -> MemberAlias {
    MemberAlias {
        id: id.into(),
        bus: id.into(),
        stems: format!("{id}.stems"),
        archived,
    }
}

fn members() -> BTreeMap<String, MemberAlias> {
    BTreeMap::from([
        ("m1".to_owned(), member("member1", false)),
        ("m2".to_owned(), member("member2", false)),
        ("old1".to_owned(), member("member1", true)),
        ("eng".to_owned(), member("engineer", false)),
    ])
}

struct World {
    dir: tempfile::TempDir,
    rpp: PathBuf,
    aliases: PathBuf,
    state: MixState,
}

impl World {
    fn new(seed: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let t = topo();
        let state = sample_state(&t, seed);
        let rpp = dir.path().join("project.RPP");
        std::fs::write(&rpp, project(&t, &state, &track_name).unwrap()).unwrap();
        let aliases = dir.path().join("aliases.toml");
        std::fs::write(&aliases, aliases_toml(&t, &members())).unwrap();
        Self {
            dir,
            rpp,
            aliases,
            state,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}

fn cmd(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|x| (*x).to_owned()).collect()
}

fn import_args(w: &World, extra: &[&str]) -> Vec<String> {
    let mut a = cmd(&["import", "--rpp"]);
    a.extend([
        s(&w.rpp),
        "--aliases".into(),
        s(&w.aliases),
        "--site".into(),
        s(&site_path()),
    ]);
    a.extend(cmd(extra));
    a
}

#[test]
fn the_synthetic_site_is_the_test_site() {
    assert_eq!(topo().diff(&synthetic_site()), Vec::<String>::new());
}

#[test]
fn import_writes_current_and_baseline_with_the_program_counts() {
    let w = World::new(1);
    let dir = w.path("state");
    let mut a = import_args(
        &w,
        &[
            "--expect",
            "tracks=45,sends=268,eqs=44,limiters=10,trims=24",
        ],
    );
    a.extend(["--state-dir".into(), s(&dir)]);
    let report = run(&a).unwrap();
    assert!(
        report.contains("counts: tracks 45, sends 268, eqs 44, limiters 10, trims 24"),
        "{report}"
    );
    assert!(report.contains("counts match --expect"));
    assert!(dir.join("current.json").exists() && dir.join("baseline.json").exists());
    let sf = site::open(&site_path()).unwrap();
    let loaded = Store::open(&dir).unwrap().load(&sf.graph);
    assert_eq!(loaded.source, Saved::Current);
    assert_eq!(loaded.persisted.topology_hash, sf.graph.hash);
    assert_eq!(
        compare(&sf.topology, &loaded.persisted.state, &w.state, 1e-9),
        Vec::<String>::new()
    );
    // A second import keeps the first as a generation.
    run(&a).unwrap();
    assert_eq!(Store::open(&dir).unwrap().generations().unwrap().len(), 1);
}

#[test]
fn a_wrong_count_an_unknown_name_or_no_state_dir_fail() {
    let w = World::new(2);
    let e = run(&import_args(&w, &["--dry-run", "--expect", "tracks=44"])).unwrap_err();
    assert_eq!(e.code, EXIT_INPUT);
    assert!(
        e.msg.contains("counts differ: tracks 45, expected 44"),
        "{}",
        e.msg
    );
    let text = std::fs::read_to_string(&w.aliases).unwrap();
    let line = format!("\"{}\" = \"keys\"\n", track_name("keys"));
    assert!(text.contains(&line));
    std::fs::write(&w.aliases, text.replace(&line, "")).unwrap();
    let e = run(&import_args(&w, &["--dry-run"])).unwrap_err();
    assert_eq!(e.code, EXIT_INPUT);
    assert!(
        e.msg.contains(&format!("{:?}", track_name("keys"))),
        "{}",
        e.msg
    );
    let e = run(&import_args(&World::new(2), &[])).unwrap_err();
    assert!(e.msg.contains("--state-dir is required"));
}

#[test]
fn a_dry_run_writes_nothing() {
    let w = World::new(3);
    let dir = w.path("state");
    let mut a = import_args(&w, &["--dry-run"]);
    a.extend(["--state-dir".into(), s(&dir)]);
    let report = run(&a).unwrap();
    assert!(report.ends_with("dry run: nothing written"), "{report}");
    assert!(!dir.exists());
}

#[test]
fn a_topology_that_differs_from_the_site_is_refused_with_its_diff() {
    let w = World::new(4);
    let site = std::fs::read_to_string(site_path()).unwrap();
    let family = "from = [\"hand1\"]\nto = [\"translator\"]";
    assert!(site.contains(family));
    let other = w.path("site.toml");
    std::fs::write(
        &other,
        site.replace(family, "from = [\"hand2\"]\nto = [\"translator\"]"),
    )
    .unwrap();
    let dir = w.path("state");
    let emitted = w.path("engine.toml");
    let mut a = cmd(&["import", "--rpp"]);
    a.extend([
        s(&w.rpp),
        "--aliases".into(),
        s(&w.aliases),
        "--site".into(),
        s(&other),
        "--state-dir".into(),
        s(&dir),
        "--emit-topology".into(),
        s(&emitted),
    ]);
    let e = run(&a).unwrap_err();
    assert_eq!(e.code, EXIT_TOPOLOGY);
    assert!(
        e.msg
            .contains("send hand1>translator: in the project, not in site.toml"),
        "{}",
        e.msg
    );
    assert!(
        e.msg
            .contains("send hand2>translator: in site.toml, not in the project"),
        "{}",
        e.msg
    );
    assert!(!dir.exists(), "nothing written");
    // The emitted [engine] table is the project's topology and compiles.
    let table = iem_engine::site::parse(&std::fs::read_to_string(emitted).unwrap()).unwrap();
    iem_engine::graph::compile(&table).unwrap();
    assert_eq!(site::topology(&table).diff(&topo()), Vec::<String>::new());
}

#[test]
fn a_site_without_an_engine_table_gets_the_projects_topology_proposed() {
    let w = World::new(12);
    let bare = w.path("site.toml");
    std::fs::write(&bare, "port = 8080\n").unwrap();
    let emitted = w.path("engine.toml");
    let mut a = cmd(&["import", "--rpp"]);
    a.extend([
        s(&w.rpp),
        "--aliases".into(),
        s(&w.aliases),
        "--site".into(),
        s(&bare),
        "--dry-run".into(),
        "--emit-topology".into(),
        s(&emitted),
    ]);
    let e = run(&a).unwrap_err();
    assert_eq!(e.code, EXIT_TOPOLOGY);
    assert!(e.msg.contains("no [engine] table yet"), "{}", e.msg);
    let table = iem_engine::site::parse(&std::fs::read_to_string(emitted).unwrap()).unwrap();
    assert_eq!(table.channels, 132);
    iem_engine::graph::compile(&table).unwrap();
    assert_eq!(site::topology(&table).diff(&topo()), Vec::<String>::new());
    let e = run(&cmd(&[
        "import",
        "--rpp",
        "x",
        "--aliases",
        "y",
        "--site",
        "z",
        "--dry-run",
    ]))
    .unwrap_err();
    assert_eq!(e.code, iem_migrate::EXIT_IO);
}

fn import_to(w: &World, dir: &Path) {
    let mut a = import_args(w, &[]);
    a.extend(["--state-dir".into(), s(dir)]);
    run(&a).unwrap();
}

fn export_args(w: &World, dir: &Path, out: &Path) -> Vec<String> {
    let mut a = cmd(&["export", "--rpp"]);
    a.extend([
        s(&w.rpp),
        "--aliases".into(),
        s(&w.aliases),
        "--site".into(),
        s(&site_path()),
        "--state-dir".into(),
        s(dir),
        "--out".into(),
        s(out),
    ]);
    a
}

#[test]
fn export_is_byte_identical_for_the_imported_state_and_never_overwrites() {
    let w = World::new(5);
    let dir = w.path("state");
    import_to(&w, &dir);
    let out = w.path("rollback.RPP");
    let report = run(&export_args(&w, &dir, &out)).unwrap();
    assert!(
        report.contains("0 line(s) changed; self-check passed"),
        "{report}"
    );
    assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&w.rpp).unwrap());
    let e = run(&export_args(&w, &dir, &out)).unwrap_err();
    assert!(e.msg.contains("never overwrites"), "{}", e.msg);
    let original = std::fs::read(&w.rpp).unwrap();
    let e = run(&export_args(&w, &dir, &w.rpp)).unwrap_err();
    assert!(e.msg.contains("never overwrites"));
    assert_eq!(
        std::fs::read(&w.rpp).unwrap(),
        original,
        "the source is untouched"
    );
}

#[test]
fn an_edited_state_exports_and_reimports_within_1e_9_db() {
    let w = World::new(6);
    let dir = w.path("state");
    import_to(&w, &dir);
    let sf = site::open(&site_path()).unwrap();
    let edited = to_mix(
        &sf.graph,
        &reconcile(&sf.graph, &sample_state(&sf.topology, 77)).0,
    );
    Store::open(&dir)
        .unwrap()
        .save(&Persisted {
            topology_hash: sf.graph.hash.clone(),
            state: edited.clone(),
            ..Persisted::default()
        })
        .unwrap();
    let out = w.path("rollback.RPP");
    run(&export_args(&w, &dir, &out)).unwrap();
    let back = w.path("back");
    let mut a = cmd(&["import", "--rpp"]);
    a.extend([
        s(&out),
        "--aliases".into(),
        s(&w.aliases),
        "--site".into(),
        s(&site_path()),
        "--state-dir".into(),
        s(&back),
    ]);
    run(&a).unwrap();
    let loaded = Store::open(&back).unwrap().load(&sf.graph);
    assert_eq!(
        compare(&sf.topology, &loaded.persisted.state, &edited, 1e-9),
        Vec::<String>::new()
    );
}

#[test]
fn export_needs_a_saved_state() {
    let w = World::new(7);
    let empty = w.path("empty");
    std::fs::create_dir(&empty).unwrap();
    let e = run(&export_args(&w, &empty, &w.path("x.RPP"))).unwrap_err();
    assert!(e.msg.contains("no saved state"), "{}", e.msg);
    let e = run(&export_args(&w, &w.path("none"), &w.path("x.RPP"))).unwrap_err();
    assert!(e.msg.contains("no state directory"), "{}", e.msg);
    assert!(!w.path("x.RPP").exists());
}

const JWT: &str = "auto-test-jwt-value";
const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
/// A PEM-shaped stand-in (no key material; built so scanners see no key).
fn key() -> String {
    format!(
        "-----BEGIN {k} KEY-----\nMIIE\n-----END {k} KEY-----\n",
        k = "PRIVATE"
    )
}

fn vapid() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32])
}

fn json<T: serde::Serialize>(path: &Path, v: &T) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string(v).unwrap()).unwrap();
}

/// A predecessor data directory with one era (t 1000…2000) over the
/// synthetic project's tracks.
fn legacy(w: &World) -> (PathBuf, PathBuf) {
    let l = w.path("legacy");
    std::fs::create_dir_all(&l).unwrap();
    std::fs::write(
        l.join("config.yaml"),
        format!(
            "port: 80\njwt_secret: \"{JWT}\"\nvapid_private_key: \"{}\"\nengineer_pin: \"8642\"\n",
            vapid()
        ),
    )
    .unwrap();
    std::fs::write(l.join("cert.pem"), CERT).unwrap();
    std::fs::write(l.join("key.pem"), key()).unwrap();
    json(
        &l.join("pins.json"),
        &HashMap::from([("m1", "1357"), ("old1", "9999")]),
    );
    let preset = PresetEntry {
        name: "rehearsal".into(),
        channels: HashMap::from([(
            1,
            ChannelPreset {
                vol: -6.0,
                mute: false,
                pan: 0.5,
            },
        )]),
        created_at: 1000,
        updated_at: 1500,
        stems_level_db: Some(-3.0),
        eq_bands: None,
    };
    json(
        &l.join("presets/m1.json"),
        &HashMap::from([("rehearsal", preset)]),
    );
    let snap = |t: i64| MixSnapshot {
        timestamp: t,
        label: "auto".into(),
        pinned: false,
        channels: HashMap::from([
            (
                1,
                ChannelSnapshot {
                    vol: 0.5,
                    mute: false,
                    pan: 0.5,
                },
            ),
            (
                18,
                ChannelSnapshot {
                    vol: 1.0,
                    mute: true,
                    pan: 0.25,
                },
            ),
        ]),
        eq_bands: None,
    };
    json(&l.join("snapshots/m1.json"), &[snap(1200), snap(1300)]);
    json(&l.join("snapshots/old1.json"), &[snap(1100)]);
    json(
        &l.join("customizations/m1.json"),
        &Customization {
            pinned: vec![1],
            hidden: vec![18],
        },
    );
    std::fs::create_dir_all(l.join("photos")).unwrap();
    std::fs::write(l.join("photos/m1.jpg"), [0xFF_u8, 0xD8, 0xFF, 0xE0]).unwrap();
    std::fs::write(l.join("photos/old1.jpg"), [0xFF_u8, 0xD8, 0xFF, 0xE1]).unwrap();
    std::fs::write(
        l.join("push_subscriptions.json"),
        r#"[{"endpoint":"https://push.example.org/1","p256dh":"p","auth":"a"}]"#,
    )
    .unwrap();
    std::fs::write(l.join("push_subs_v2_migrated"), b"").unwrap();
    json(
        &l.join("backups/2026-01-01.json"),
        &serde_json::json!({"version": 1, "pins": {"m1": "1357"}, "sends": []}),
    );
    let t = topo();
    let names: Vec<String> = t
        .inputs
        .iter()
        .map(|i| format!("{:?}", track_name(&i.id.0)))
        .chain(
            t.buses
                .iter()
                .filter(|b| b.id.0 != "master")
                .map(|b| format!("{:?}", track_name(&b.id.0))),
        )
        .collect();
    let eras = w.path("eras.toml");
    std::fs::write(
        &eras,
        format!(
            "[[era]]\nfirst_seen = 1000\nlast_seen = 2000\ntracks = [{}]\n",
            names.join(", ")
        ),
    )
    .unwrap();
    (l, eras)
}

fn band_args(w: &World, l: &Path, eras: &Path, out: &Path, extra: &[&str]) -> Vec<String> {
    let defaults = w.path("defaults.txt");
    std::fs::write(&defaults, "member=2468\n").unwrap();
    let mut a = cmd(&["band", "--legacy"]);
    a.extend([
        s(l),
        "--aliases".into(),
        s(&w.aliases),
        "--eras".into(),
        s(eras),
        "--site".into(),
        s(&site_path()),
        "--out".into(),
        s(out),
        "--legacy-default-pins".into(),
        s(&defaults),
    ]);
    a.extend(cmd(extra));
    a
}

fn hasher(out: &Path) -> (PinHasher, PinStore) {
    let secrets = out.join("secrets");
    (
        PinHasher::new(pepper::load_or_create(&secrets).unwrap()),
        PinStore::load(&secrets).unwrap(),
    )
}

#[test]
fn band_data_moves_with_the_same_pins_secrets_and_certificate() {
    let w = World::new(8);
    let (l, eras) = legacy(&w);
    let out = w.path("band");
    let dry = run(&band_args(&w, &l, &eras, &out, &["--dry-run"])).unwrap();
    assert!(dry.contains("dry run: nothing written"), "{dry}");
    assert!(!out.exists(), "a dry run writes nothing");
    let report = run(&band_args(&w, &l, &eras, &out, &[])).unwrap();
    for secret in [JWT, "1357", "2468", "8642", "9999", vapid().as_str()] {
        assert!(
            !report.contains(secret) && !dry.contains(secret),
            "the report shows a secret"
        );
    }
    assert!(
        report.contains("pins: 1 member(s) with their own PIN, 1 on the predecessor's default PIN"),
        "{report}"
    );
    let presets: PresetFile =
        serde_json::from_str(&std::fs::read_to_string(out.join("presets/member1.json")).unwrap())
            .unwrap();
    assert_eq!(presets.presets.len(), 1);
    assert_eq!(
        presets.presets[0].sends[0].src,
        Source::Input(InputId::new("mic1"))
    );
    assert_eq!(presets.presets[0].stems_fader_db, Some(-3.0));
    let snaps: SnapshotFile =
        serde_json::from_str(&std::fs::read_to_string(out.join("snapshots/member1.json")).unwrap())
            .unwrap();
    assert_eq!(snaps.snapshots.len(), 3);
    assert_eq!(snaps.snapshots.iter().filter(|x| x.archived).count(), 1);
    assert_eq!(
        snaps.snapshots[0].sends[1].src,
        Source::Input(InputId::new("click"))
    );
    let c: CustomizationFile = serde_json::from_str(
        &std::fs::read_to_string(out.join("customizations/member1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(c.hidden, vec![Source::Input(InputId::new("click"))]);
    assert_eq!(
        std::fs::read(out.join("photos/member1.jpg")).unwrap(),
        vec![0xFF, 0xD8, 0xFF, 0xE0]
    );
    assert!(!out.join("photos/old1.jpg").exists());
    let (h, store) = hasher(&out);
    assert!(h.verify("1357", store.member_hash("member1").unwrap()));
    assert!(h.verify("2468", store.member_hash("member2").unwrap()));
    assert!(h.verify("8642", store.engineer_hash().unwrap()));
    assert!(store.member_hash("engineer").is_none());
    let loaded = iem_server::secrets::load_or_create(&out.join("secrets")).unwrap();
    assert_eq!(loaded.jwt_secret, JWT);
    assert_eq!(loaded.vapid_private_key, vapid());
    assert_eq!(std::fs::read_to_string(out.join("cert.pem")).unwrap(), CERT);
    assert_eq!(std::fs::read_to_string(out.join("key.pem")).unwrap(), key());
    assert!(out.join("push_subs_v2_migrated").exists());
    assert!(
        std::fs::read_to_string(out.join("push_subscriptions.json"))
            .unwrap()
            .contains("push.example.org/1")
    );
    let archived = std::fs::read_to_string(out.join("legacy/backups/2026-01-01.json")).unwrap();
    assert!(
        !archived.contains("pins") && !archived.contains("1357"),
        "{archived}"
    );
}

#[test]
fn a_later_band_import_keeps_a_pin_set_in_iemmixer() {
    let w = World::new(9);
    let (l, eras) = legacy(&w);
    let out = w.path("band");
    run(&band_args(&w, &l, &eras, &out, &[])).unwrap();
    let (h, mut store) = hasher(&out);
    store.set_member_hash("member1", h.hash("1122")).unwrap();
    let report = run(&band_args(&w, &l, &eras, &out, &[])).unwrap();
    assert!(
        report.contains("pin member1: kept (set in iemmixer)"),
        "{report}"
    );
    assert!(report.contains("pin member2: unchanged"), "{report}");
    let (h, store) = hasher(&out);
    assert!(h.verify("1122", store.member_hash("member1").unwrap()));
}

#[test]
fn missing_or_unmappable_band_data_fails_loudly() {
    let w = World::new(10);
    let (l, eras) = legacy(&w);
    let out = w.path("band");
    std::fs::remove_file(l.join("config.yaml")).unwrap();
    let e = run(&band_args(&w, &l, &eras, &out, &["--dry-run"])).unwrap_err();
    assert!(e.msg.contains("config.yaml is missing"), "{}", e.msg);
    let partial = run(&band_args(&w, &l, &eras, &out, &["--dry-run", "--partial"])).unwrap();
    assert!(partial.contains("absent: config.yaml"), "{partial}");
    json(
        &l.join("snapshots/stranger.json"),
        &Vec::<MixSnapshot>::new(),
    );
    let e = run(&band_args(&w, &l, &eras, &out, &["--partial"])).unwrap_err();
    assert!(
        e.msg.contains("member \"stranger\" is not in the aliases"),
        "{}",
        e.msg
    );
    assert!(!out.exists(), "nothing written");
    std::fs::remove_file(l.join("snapshots/stranger.json")).unwrap();
    let late = MixSnapshot {
        timestamp: 1500,
        label: "x".into(),
        pinned: false,
        channels: HashMap::from([(
            999,
            ChannelSnapshot {
                vol: 1.0,
                mute: false,
                pan: 0.5,
            },
        )]),
        eq_bands: None,
    };
    json(&l.join("snapshots/m2.json"), &[late]);
    let e = run(&band_args(&w, &l, &eras, &out, &["--partial"])).unwrap_err();
    assert!(e.msg.contains("snapshots/m2.json: snapshot 1"), "{}", e.msg);
}

fn bin(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_iem-migrate"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_binary_reports_through_exit_codes() {
    let help = bin(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("iem-migrate import"));
    let none = bin(&[]);
    assert_eq!(none.status.code(), Some(i32::from(EXIT_INPUT)));
    assert!(String::from_utf8_lossy(&none.stderr).starts_with("iem-migrate: "));
    let w = World::new(11);
    let a = import_args(&w, &["--dry-run"]);
    let refs: Vec<&str> = a.iter().map(String::as_str).collect();
    let ok = bin(&refs);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("dry run: nothing written"));
}
