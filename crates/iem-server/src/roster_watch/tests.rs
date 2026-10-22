//! Loading and reloading the roster (#58 design §9).

use super::*;

use iem_core::LineupRef;
use iem_engine_proto::{InputId, Level, Mix};

use crate::engine::client::fake;
use crate::roster_state::tests::test_roster;
use crate::site_view::tests::{test_config, test_topology};

/// The test roster's text (`config/test-roster.toml`).
const TEXT: &str = include_str!("../../../../config/test-roster.toml");

/// A PHC string of the right kind (the store checks the prefix only).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo";

/// `text` with `from` (found exactly once) replaced by `to`.
fn edit(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{from}");
    text.replacen(from, to, 1)
}

fn parse(text: &str) -> Roster {
    let roster = Roster::parse(text).unwrap();
    assert_eq!(roster.problems(), Vec::<String>::new());
    roster
}

/// The test roster without member11 (who left: youth's set member4 is
/// empty, mic4 is youth's no more).
fn without_member11() -> String {
    let t = edit(
        TEXT,
        "[[people]]\nid = \"member11\"\nname = \"Member 11\"\n\n",
        "",
    );
    let t = edit(&t, "member4 = \"member11\"\n", "");
    edit(
        &t,
        "mic4 = { name = \"MEMBER11 mic\", owner = \"member11\" }\n",
        "",
    )
}

/// The test roster where member5 wears set member9 in main (member9 plays
/// in no lineup, mic10 has no owner there).
fn moved() -> String {
    let t = edit(TEXT, "member5 = \"member5\"\n", "");
    let t = edit(&t, "member9 = \"member9\"\n", "member9 = \"member5\"\n");
    edit(
        &t,
        "mic10 = { name = \"MEMBER9 mic\", owner = \"member9\" }\n",
        "mic10 = { name = \"MEMBER9 mic\" }\n",
    )
}

/// The test roster where member5 and member6 swap sets in main and
/// member10 wears set member2 in youth.
fn swapped() -> String {
    let t = edit(
        TEXT,
        "member5 = \"member5\"\nmember6 = \"member6\"\n",
        "member5 = \"member6\"\nmember6 = \"member5\"\n",
    );
    edit(&t, "member1 = \"member10\"\n", "member2 = \"member10\"\n")
}

fn pair(from: &str, to: &str) -> (String, String) {
    (from.to_string(), to.to_string())
}

fn level(gain_db: f64) -> Level {
    Level {
        gain_db,
        pan: 0.0,
        muted: false,
    }
}

fn mix_mut<'a>(s: &'a mut MixState, id: &str) -> &'a mut Mix {
    s.mixes.get_mut(&MixId::new(id)).unwrap()
}

/// A running state: every mix of the test site; member5's keys at −6 dB,
/// member9's at −20 dB; member1 hears member2 and member3 hears member1 at
/// 0 dB.
fn running() -> MixState {
    let mut s = MixState::default();
    for m in test_topology().mixes {
        s.mixes.insert(m.id, Mix::default());
    }
    let keys = InputId::new("keys");
    mix_mut(&mut s, "member5")
        .inputs
        .insert(keys.clone(), level(-6.0));
    mix_mut(&mut s, "member9").inputs.insert(keys, level(-20.0));
    mix_mut(&mut s, "member1")
        .mixes
        .insert(MixId::new("member2"), level(0.0));
    mix_mut(&mut s, "member3")
        .mixes
        .insert(MixId::new("member1"), level(0.0));
    s
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn status(dir: &Path) -> Status {
    serde_json::from_str(&read(&dir.join(STATUS_FILE))).unwrap()
}

/// A state over `dir` with the test roster in use, `active` playing.
fn with_roster(dir: &Path, active: &str) -> AppState {
    let state = AppState::new(test_config(), dir);
    state.set_roster(Some(test_roster(active)));
    state
}

/// Connects `state` to a fake engine whose mirror holds `running` at
/// revision 1; the engine's end.
async fn synced(state: &mut AppState, running: MixState) -> fake::Peer {
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    fake::sync(&state.engine, &mut peer, 1, running).await;
    peer
}

/// Answers the next request but the freeze's barrier `Ping`s, an
/// `ImportState`, as the engine does (the reply at revision 2, then the
/// state); what it imported.
async fn imports(peer: &mut fake::Peer) -> MixState {
    fake::imports(peer, 2).await
}

/// Refuses the next request but `Ping`s, an `ImportState`.
async fn refuses(peer: &mut fake::Peer) {
    fake::refuses(peer).await;
}

/// [`apply_text`] of `text` on its own task.
fn spawn_apply(state: &AppState, text: &str) -> tokio::task::JoinHandle<Applied> {
    let (s, text) = (state.clone(), text.to_string());
    tokio::spawn(async move {
        let sha = sha256_hex(text.as_bytes());
        apply_text(&s, &text, &sha).await
    })
}

async fn within<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("within 5 s")
        .expect("the task")
}

fn applied(added: &[&str], removed: &[&str]) -> Applied {
    let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
    Applied::Applied {
        added: ids(added),
        removed: ids(removed),
        problems: Vec::new(),
    }
}

#[test]
fn sha256_is_the_hex_of_the_bytes() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_stamp_tells_a_changed_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(ROSTER_FILE);
    assert_eq!(stamp(&path), None);
    std::fs::write(&path, "a").unwrap();
    let one = stamp(&path);
    assert_eq!(one.map(|(len, _)| len), Some(1));
    std::fs::write(&path, "bb").unwrap();
    assert_ne!(stamp(&path), one);
}

#[test]
fn validate_refuses_what_does_not_parse_has_problems_or_does_not_fit() {
    let facts = TopologyFacts::from(&test_topology());
    assert_eq!(validate(TEXT, Some(&facts)), Ok(parse(TEXT)));
    assert_eq!(validate(TEXT, None), Ok(parse(TEXT)));
    let bad = validate("[[people]]\n", None).unwrap_err();
    assert_eq!(bad.len(), 1);
    assert!(bad[0].starts_with("roster file: "), "{bad:?}");
    let two = edit(
        TEXT,
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
    );
    assert_eq!(
        validate(&two, None),
        Err(vec!["exactly one lineup must be main, not 2".to_string()])
    );
    let mut topo = test_topology();
    let m1 = topo
        .mixes
        .iter_mut()
        .find(|m| m.id == MixId::new("member1"))
        .unwrap();
    m1.peers.retain(|p| *p != MixId::new("member3"));
    assert_eq!(
        validate(TEXT, Some(&TopologyFacts::from(&topo))),
        Err(vec![
            "leader member1 in main: set member1 does not hear member3; the site needs peers (F30)"
                .to_string()
        ])
    );
}

#[test]
fn moves_name_who_wears_another_set() {
    let old = parse(TEXT);
    let new = parse(&moved());
    assert_eq!(moves(&old, &new, "main"), [pair("member5", "member9")]);
    assert!(moves(&old, &new, "youth").is_empty());
    assert!(moves(&old, &old, "main").is_empty());
    assert!(moves(&old, &new, "gone").is_empty());
    let swap = parse(&swapped());
    assert_eq!(
        moves(&old, &swap, "main"),
        [pair("member6", "member5"), pair("member5", "member6")]
    );
    assert_eq!(moves(&old, &swap, "youth"), [pair("member1", "member2")]);
    // A lineup the old roster lacks moves nobody.
    let no_youth = parse(TEXT.split("[[lineups]]\nid = \"youth\"").next().unwrap());
    assert!(moves(&no_youth, &swap, "youth").is_empty());
}

#[test]
fn a_moved_mix_is_copied_from_before_any_move() {
    let mut s = running();
    s.mixes
        .get_mut(&MixId::new("member5"))
        .unwrap()
        .mixes
        .insert(MixId::new("member6"), level(-4.0));
    let keys = |st: &MixState, m: &str| {
        st.mixes[&MixId::new(m)]
            .inputs
            .get(&InputId::new("keys"))
            .map(|l| l.gain_db)
    };
    let mut swap = s.clone();
    apply_moves(
        &mut swap,
        &[pair("member5", "member6"), pair("member6", "member5")],
    );
    assert_eq!(keys(&swap, "member6"), Some(-6.0), "member5's mix");
    assert_eq!(keys(&swap, "member5"), None, "member6's mix had no keys");
    // member5 heard member6 at −4 dB; on member6's set it hears set member5,
    // whom it swapped with, at −4 dB, and never itself.
    let m6 = &swap.mixes[&MixId::new("member6")];
    assert_eq!(
        m6.mixes.get(&MixId::new("member5")).map(|l| l.gain_db),
        Some(-4.0)
    );
    assert!(!m6.mixes.contains_key(&MixId::new("member6")));
    // A vacated set keeps its mix; a set without a mix moves nothing.
    let mut one = s.clone();
    apply_moves(
        &mut one,
        &[pair("member5", "member9"), pair("ghost", "member2")],
    );
    assert_eq!(keys(&one, "member9"), Some(-6.0));
    assert_eq!(keys(&one, "member5"), Some(-6.0));
    assert_eq!(
        one.mixes[&MixId::new("member2")],
        s.mixes[&MixId::new("member2")]
    );
}

/// #58 design §7: only a leader hears another person's mix; a set nobody
/// wears is silent.
#[test]
fn fit_mutes_heard_mixes_off_a_leaders_set_and_silences_empty_sets() {
    let r = test_roster("youth");
    let mut s = running();
    mix_mut(&mut s, "member4")
        .mixes
        .insert(MixId::new("member3"), level(-3.0));
    mix_mut(&mut s, "engineer")
        .mixes
        .insert(MixId::new("member1"), level(-3.0));
    fit_recorded(
        &mut s,
        &r.roster,
        r.roster.lineup("youth").unwrap(),
        &mut BTreeSet::new(),
    );
    let mix = |id: &str| &s.mixes[&MixId::new(id)];
    let of = |id: &str, h: &str| mix(id).mixes[&MixId::new(h)].muted;
    assert!(of("member1", "member2"), "member10 leads no part of youth");
    assert!(of("member4", "member3"), "nor does member11");
    assert!(!of("member3", "member1"), "member3 leads youth");
    assert!(!of("engineer", "member1"), "the engineer's mix is no set");
    for worn in ["member1", "member3", "member4", "engineer", "translator"] {
        assert!(!mix(worn).out.muted, "{worn}");
    }
    for vacant in [
        "member2", "member5", "member6", "member7", "member8", "member9",
    ] {
        assert!(mix(vacant).out.muted, "{vacant}");
    }
    // Main: both leaders keep what they hear and every set is worn.
    let mut main = running();
    fit_recorded(
        &mut main,
        &r.roster,
        r.roster.lineup("main").unwrap(),
        &mut BTreeSet::new(),
    );
    assert_eq!(main, running());
    // A mix the state does not hold is not made up.
    let mut empty = MixState::default();
    fit_recorded(
        &mut empty,
        &r.roster,
        r.roster.lineup("youth").unwrap(),
        &mut BTreeSet::new(),
    );
    assert_eq!(empty, MixState::default());
}

/// #58 review: the record of what the rules silenced: a set they mute is
/// recorded, a set muted already is not, a recorded set someone wears is
/// unmuted and forgotten; a moved mix takes its set's record along.
#[test]
fn the_rules_record_what_they_silenced() {
    let r = test_roster("youth");
    let youth = r.roster.lineup("youth").unwrap();
    let mut s = running();
    mix_mut(&mut s, "member5").out.muted = true;
    let mut silenced = BTreeSet::new();
    fit_recorded(&mut s, &r.roster, youth, &mut silenced);
    let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(
        silenced,
        set(&["member2", "member6", "member7", "member8", "member9"]),
        "member5 was muted already"
    );
    let main = r.roster.lineup("main").unwrap();
    fit_recorded(&mut s, &r.roster, main, &mut silenced);
    assert!(silenced.is_empty());
    for id in ["member2", "member6", "member7", "member8", "member9"] {
        assert!(!s.mixes[&MixId::new(id)].out.muted, "{id}");
    }
    assert!(s.mixes[&MixId::new("member5")].out.muted, "its wearer's");
    // A set takes the record of the set whose mix it takes.
    let mut silenced = set(&["member5", "member9"]);
    move_silenced(
        &mut silenced,
        &[pair("member5", "member6"), pair("member1", "member9")],
    );
    assert_eq!(silenced, set(&["member5", "member6"]));
}

#[test]
fn the_people_diff_names_who_came_and_who_left() {
    let (old, new) = (parse(TEXT), parse(&without_member11()));
    assert_eq!(
        people_diff(&old, &new),
        (Vec::new(), vec!["member11".to_string()])
    );
    assert_eq!(
        people_diff(&new, &old),
        (vec!["member11".to_string()], Vec::new())
    );
    assert_eq!(people_diff(&old, &old), (Vec::new(), Vec::new()));
}

/// #58 decision 6: who left goes to the archive, who came back comes out of
/// it; a failed move is named and the rest still runs.
#[test]
fn follow_people_archives_who_left_and_restores_who_came_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    for id in ["member2", "member11", "stale"] {
        pins.set_member_hash(id, PHC.into()).unwrap();
    }
    let file = |id: &str| {
        d.join("lineups")
            .join("youth")
            .join("presets")
            .join(format!("{id}.json"))
    };
    put(&file("member11"), "member11's");
    put(&file("member12"), "member12's");
    let without = parse(&without_member11());
    let removed = ["member11".to_string(), "member12".to_string()];
    let p = follow_people(d, &without, &mut pins, &removed);
    assert_eq!(
        p,
        People {
            restored: Vec::new(),
            archived: vec!["member11".into(), "member12".into(), "stale".into()],
            problems: Vec::new(),
        },
        "the removed, then a live PIN of no person"
    );
    assert!(!file("member11").exists());
    assert_eq!(pins.member_hash("member11"), None);
    assert_eq!(pins.member_hash("member2"), Some(PHC), "a person stays");
    let full = parse(TEXT);
    // member9's PIN alone is archived (nothing else of theirs was): it
    // comes back too.
    pins.set_member_hash("member9", PHC.into()).unwrap();
    assert!(pins.archive_member("member9").unwrap());
    assert!(
        !d.join(crate::people_archive::ARCHIVE_DIR)
            .join("member9")
            .exists()
    );
    let back = follow_people(d, &full, &mut pins, &[]);
    assert_eq!(
        back,
        People {
            restored: vec!["member9".into(), "member11".into()],
            archived: Vec::new(),
            problems: Vec::new(),
        }
    );
    assert_eq!(read(&file("member11")), "member11's");
    assert_eq!(pins.member_hash("member11"), Some(PHC));
    assert_eq!(pins.member_hash("member9"), Some(PHC));
    assert!(!file("member12").exists(), "no person: still archived");
    assert_eq!(follow_people(d, &full, &mut pins, &[]), People::default());
    // member14's archive is blocked (a file where its directory belongs):
    // named, and member15 is archived all the same.
    put(&file("member14"), "member14's");
    put(&file("member15"), "member15's");
    put(
        &d.join(crate::people_archive::ARCHIVE_DIR)
            .join("member14")
            .join("lineups")
            .join("youth")
            .join("presets"),
        "in the way",
    );
    let gone = ["member14".to_string(), "member15".to_string()];
    let p = follow_people(d, &full, &mut pins, &gone);
    assert_eq!(p.archived, ["member15"]);
    assert_eq!(p.problems.len(), 1, "{p:?}");
    assert!(
        p.problems[0].starts_with("person 'member14' was not archived: "),
        "{p:?}"
    );
    assert!(file("member14").exists());
}

#[test]
fn load_migrates_starts_on_main_and_refuses_what_it_cannot_use() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let cfg = test_config();
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    assert_eq!(load(d, &cfg, &mut pins, None), Ok(None), "legacy mode");
    assert!(!d.join(STATUS_FILE).exists());
    std::fs::write(d.join(ROSTER_FILE), [0xff, 0xfe]).unwrap();
    assert_eq!(
        load(d, &cfg, &mut pins, None),
        Err("roster.toml: roster file: not UTF-8 text".to_string())
    );
    let s = status(d);
    assert_eq!((s.ok, s.applied), (false, false));
    assert_eq!(s.sha256, sha256_hex(&[0xff, 0xfe]));
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut topo = test_topology();
    topo.mixes
        .iter_mut()
        .find(|m| m.id == MixId::new("member1"))
        .unwrap()
        .peers
        .clear();
    let err = load(d, &cfg, &mut pins, Some(&TopologyFacts::from(&topo))).unwrap_err();
    assert!(err.contains("set member1 does not hear member2"), "{err}");
    assert!(
        !d.join(crate::lineup_store::LINEUPS_DIR).exists(),
        "nothing migrated"
    );
    // A former member with a live PIN goes to the archive with the migration.
    pins.set_member_hash("member9", PHC.into()).unwrap();
    let mut cfg = cfg;
    cfg.members.push(iem_core::SiteMember {
        id: "former".into(),
        name: "Former".into(),
        mix: "member9".into(),
    });
    pins.set_member_hash("former", PHC.into()).unwrap();
    let facts = TopologyFacts::from(&test_topology());
    let loaded = load(d, &cfg, &mut pins, Some(&facts))
        .unwrap()
        .expect("roster mode");
    assert!(!loaded.fallback, "roster.toml itself");
    let r = loaded.roster;
    assert_eq!(
        (r.active.as_str(), r.sha256.as_str()),
        ("main", sha256_hex(TEXT.as_bytes()).as_str())
    );
    assert!(d.join(ACTIVE_FILE).exists());
    assert_eq!(pins.member_hash("former"), None);
    assert!(pins.member_hash("member9").is_some());
    let s = status(d);
    assert!(s.ok && s.applied, "{s:?}");
    assert_eq!(s.removed, ["former"]);
}

#[tokio::test]
async fn a_person_who_moves_sets_takes_their_mix() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let mut peer = synced(&mut state, running()).await;
    let text = moved();
    let task = spawn_apply(&state, &text);
    let imported = imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    let mix = |id: &str| &imported.mixes[&MixId::new(id)];
    let keys = |id: &str| mix(id).inputs[&InputId::new("keys")].gain_db;
    assert_eq!(keys("member9"), -6.0, "member5's mix is on set member9");
    assert!(!mix("member9").out.muted);
    assert_eq!(keys("member5"), -6.0, "the vacated set keeps its mix");
    assert!(mix("member5").out.muted, "nobody wears it in main now");
    assert!(
        !mix("member1").mixes[&MixId::new("member2")].muted,
        "a leader keeps what they hear"
    );
    assert_eq!(state.engine.mirror().state, imported);
    let r = state.roster().unwrap();
    assert_eq!(r.active, "main");
    assert_eq!(r.sha256, sha256_hex(text.as_bytes()));
    assert_eq!(r.lineup().person_on("member9"), Some("member5"));
}

/// #58 third review: every roster that comes into use is kept, by the
/// start's load and by a reload, and a reload deletes none kept before.
#[tokio::test]
async fn every_roster_in_use_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    let facts = TopologyFacts::from(&test_topology());
    load(d, &test_config(), &mut pins, Some(&facts))
        .unwrap()
        .expect("roster mode");
    let first = sha256_hex(TEXT.as_bytes());
    assert_eq!(store.kept_roster(&first).unwrap().as_deref(), Some(TEXT));
    let mut state = with_roster(d, "main");
    let mut peer = synced(&mut state, running()).await;
    let text = moved();
    let task = spawn_apply(&state, &text);
    imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    let second = sha256_hex(text.as_bytes());
    assert_eq!(
        store.kept_roster(&second).unwrap().as_deref(),
        Some(text.as_str())
    );
    assert_eq!(store.kept_roster(&first).unwrap().as_deref(), Some(TEXT));
}

#[tokio::test]
async fn a_valid_change_applies_archives_the_removed_and_announces_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    // A reload needs the engine synced (nothing of main changes here).
    let _peer = synced(&mut state, MixState::default()).await;
    let mut rx = state.lineup_tx.subscribe();
    state
        .pin_store
        .write()
        .await
        .set_member_hash("member11", PHC.into())
        .unwrap();
    let file = d
        .join("lineups")
        .join("youth")
        .join("presets")
        .join("member11.json");
    put(&file, "member11's");
    let text = without_member11();
    std::fs::write(d.join(ROSTER_FILE), &text).unwrap();
    assert_eq!(check_file(&state).await, Some(applied(&[], &["member11"])));
    let s = status(d);
    assert!(s.ok && s.applied, "{s:?}");
    assert_eq!(
        (s.sha256.as_str(), s.removed.as_slice()),
        (
            sha256_hex(text.as_bytes()).as_str(),
            &["member11".to_string()][..]
        )
    );
    assert_eq!(state.roster().unwrap().sha256, sha256_hex(text.as_bytes()));
    assert!(state.roster().unwrap().roster.person("member11").is_none());
    assert_eq!(state.pin_store.read().await.member_hash("member11"), None);
    assert!(!file.exists(), "archived");
    let main = LineupRef {
        id: "main".into(),
        name: "Main band".into(),
    };
    assert_eq!(rx.try_recv().unwrap(), main);
    // The person comes back with their files and PIN.
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    assert_eq!(check_file(&state).await, Some(applied(&["member11"], &[])));
    assert_eq!(status(d).added, ["member11"]);
    assert_eq!(read(&file), "member11's");
    assert_eq!(
        state.pin_store.read().await.member_hash("member11"),
        Some(PHC)
    );
    assert_eq!(rx.try_recv().unwrap(), main);
}

#[tokio::test]
async fn a_roster_without_the_active_lineup_is_refused_and_the_old_one_kept() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let state = with_roster(d, "youth");
    let mut rx = state.lineup_tx.subscribe();
    let text = TEXT.split("[[lineups]]\nid = \"youth\"").next().unwrap();
    std::fs::write(d.join(ROSTER_FILE), text).unwrap();
    let why = "the roster has no lineup 'youth', which plays: switch to a lineup it keeps first";
    assert_eq!(
        check_file(&state).await,
        Some(Applied::Refused(vec![why.to_string()]))
    );
    assert_eq!(state.roster(), Some(test_roster("youth")));
    let s = status(d);
    assert_eq!(
        (s.ok, s.applied, s.problems.as_slice()),
        (false, false, &[why.to_string()][..])
    );
    assert!(rx.try_recv().is_err(), "nothing announced");
}

#[tokio::test]
async fn a_problem_roster_writes_a_refusing_status() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    let two = edit(
        TEXT,
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
    );
    std::fs::write(d.join(ROSTER_FILE), &two).unwrap();
    let why = vec!["exactly one lineup must be main, not 2".to_string()];
    assert_eq!(
        check_file(&state).await,
        Some(Applied::Refused(why.clone()))
    );
    assert_eq!(
        status(d),
        Status {
            at: status(d).at,
            ..Status::refused(&sha256_hex(two.as_bytes()), why)
        }
    );
    assert_eq!(state.roster(), Some(test_roster("main")));
    // With the engine's topology known, a roster that does not fit it.
    let mut topo = test_topology();
    topo.mixes
        .iter_mut()
        .find(|m| m.id == MixId::new("member1"))
        .unwrap()
        .peers
        .retain(|p| *p != MixId::new("member3"));
    let (engine, _peer) = fake::announced(topo).await;
    state.engine = engine;
    assert_eq!(
        apply_text(&state, TEXT, "edited").await,
        Applied::Refused(vec![
            "leader member1 in main: set member1 does not hear member3; the site needs peers (F30)"
                .to_string()
        ])
    );
    // Text that is not UTF-8.
    std::fs::write(d.join(ROSTER_FILE), [0xff]).unwrap();
    assert_eq!(
        check_file(&state).await,
        Some(Applied::Refused(vec![NOT_UTF8.to_string()]))
    );
    assert_eq!(status(d).sha256, sha256_hex(&[0xff]));
    // A file that is gone changes nothing.
    std::fs::remove_file(d.join(ROSTER_FILE)).unwrap();
    assert_eq!(check_file(&state).await, None);
    assert_eq!(state.roster(), Some(test_roster("main")));
}

#[tokio::test]
async fn an_unchanged_file_is_not_applied_again() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let state = AppState::new(test_config(), d);
    let mut r = test_roster("main");
    r.sha256 = sha256_hex(TEXT.as_bytes());
    state.set_roster(Some(r.clone()));
    let mut rx = state.lineup_tx.subscribe();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    assert_eq!(check_file(&state).await, Some(Applied::Unchanged));
    assert_eq!(
        apply_text(&state, TEXT, &r.sha256).await,
        Applied::Unchanged
    );
    assert!(!d.join(STATUS_FILE).exists(), "no status written");
    assert!(rx.try_recv().is_err(), "nothing announced");
    assert_eq!(state.roster(), Some(r));
}

/// A reload the engine refuses changes nothing: every stored state it had
/// written goes back.
#[tokio::test]
async fn a_refused_import_puts_the_stored_states_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let mut youth = running();
    youth
        .mixes
        .get_mut(&MixId::new("member1"))
        .unwrap()
        .inputs
        .insert(InputId::new("keys"), level(-3.0));
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, youth);
    store.save_state("youth", &stored).unwrap();
    let mut peer = synced(&mut state, running()).await;
    let task = spawn_apply(&state, &swapped());
    refuses(&mut peer).await;
    let result = within(task).await;
    assert!(
        matches!(&result, Applied::Refused(p) if p.len() == 1
            && p[0].starts_with("the engine did not take lineup main's new mixes: ")),
        "{result:?}"
    );
    assert_eq!(store.state("youth").unwrap(), Some(stored), "put back");
    assert_eq!(state.roster(), Some(test_roster("main")));
}

/// #58 review: every reload waits for a synced engine, also one that moves
/// nobody (the rules must reach the running state with it), and no stored
/// state is rewritten while it waits.
#[tokio::test]
async fn a_reload_waits_for_a_synced_engine_even_without_a_move() {
    let dir = tempfile::tempdir().unwrap();
    let state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    store
        .save_state("youth", &MixerBackup::new("t".into(), 1, running()))
        .unwrap();
    let file = store.data_dir("youth").join("state.json");
    let before = (
        std::fs::read(&file).unwrap(),
        std::fs::metadata(&file).unwrap().modified().unwrap(),
    );
    // member11 leaves: youth's set member4 is nobody's, nothing moves.
    let text = without_member11();
    assert_eq!(
        apply_text(&state, &text, &sha256_hex(text.as_bytes())).await,
        Applied::Deferred(vec![
            "the engine state is not synced: the roster waits for it".to_string()
        ])
    );
    let after = (
        std::fs::read(&file).unwrap(),
        std::fs::metadata(&file).unwrap().modified().unwrap(),
    );
    assert_eq!(after, before, "youth's stored state untouched");
    assert_eq!(state.roster(), Some(test_roster("main")));
}

/// Waits (≤ 5 s) until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !done() {
        assert!(t0.elapsed() < Duration::from_secs(5), "{what} within 5 s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// #58 review: the roster a server starts with reaches the running state
/// once the engine has synced (youth's empty sets silent, the heard mixes
/// of a set no leader wears off); until then no page changes a mix, and
/// then the roster is recorded as the one the states follow.
#[tokio::test]
async fn the_rules_reach_the_running_state_after_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    LineupStore::new(d).set_active("youth", 1).unwrap();
    let mut state = AppState::new(test_config(), d);
    assert_eq!(state.roster().unwrap().active, "youth");
    let page = state.page("member3").unwrap();
    assert_eq!(
        state.may_change(&page),
        Err(crate::view::ViewError::Switching),
        "not settled yet"
    );
    let mut peer = synced(&mut state, running()).await;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    let imported = imports(&mut peer).await;
    within(settle).await;
    let mix = |id: &str| &imported.mixes[&MixId::new(id)];
    assert!(
        mix("member2").out.muted,
        "nobody wears set member2 in youth"
    );
    assert!(!mix("member3").out.muted);
    assert!(
        mix("member1").mixes.values().all(|l| l.muted),
        "member10 leads nothing"
    );
    assert!(!mix("member3").mixes[&MixId::new("member1")].muted);
    let page = state.page("member3").unwrap();
    assert_eq!(state.may_change(&page), Ok(()));
    assert_eq!(
        LineupStore::new(d).applied_roster().unwrap().as_deref(),
        Some(TEXT),
        "the roster the states follow"
    );
}

/// #58, the owner's look at the live system: the rules silence a departed
/// person's set (member9: no lineup lists it, the site's legacy members
/// still name it) as any set nobody wears, and record it, so it sounds
/// again only once a roster puts someone on it. The engineer's and the
/// translator's mixes are no person's set and stay as they were.
#[tokio::test]
async fn the_rules_silence_a_departed_persons_set() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(
        d.join(ROSTER_FILE),
        crate::roster_state::tests::departed_text(),
    )
    .unwrap();
    LineupStore::new(d).set_active("youth", 1).unwrap();
    let mut state = AppState::new(test_config(), d);
    let mut peer = synced(&mut state, running()).await;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    let imported = imports(&mut peer).await;
    within(settle).await;
    let muted = |id: &str| imported.mixes[&MixId::new(id)].out.muted;
    assert!(muted("member9"), "member9 left: nobody wears their set");
    assert!(muted("member2"), "nobody wears set member2 in youth");
    for kept in ["member1", "member3", "member4", "engineer", "translator"] {
        assert!(!muted(kept), "{kept}");
    }
    let running = LineupStore::new(d).running().unwrap().unwrap();
    assert!(running.silenced.contains("member9"), "{running:?}");
    assert!(!running.silenced.contains("engineer"));
    assert!(!running.silenced.contains("translator"));
}

/// #58 review: a legacy server that adopts its first roster with the
/// engine synced puts the main lineup's rules into the running state.
#[tokio::test]
async fn an_adopted_roster_reaches_the_running_state() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = AppState::new(test_config(), d);
    let mut peer = synced(&mut state, running()).await;
    // The roster where member3 leads nothing in main: set member3, which
    // hears its peers, is worn by no leader.
    let text = edit(
        TEXT,
        "leaders = [\"member1\", \"member3\"]\n",
        "leaders = [\"member1\"]\n",
    );
    std::fs::write(d.join(ROSTER_FILE), text).unwrap();
    assert_eq!(check_file(&state).await, Some(applied(&[], &[])));
    let imported = imports(&mut peer).await;
    let set3 = &imported.mixes[&MixId::new("member3")].mixes;
    assert!(set3[&MixId::new("member1")].muted, "member1 at 0 dB before");
    assert_eq!(set3.len(), 8, "its eight peers");
    assert!(
        set3.values().all(|l| l.muted),
        "a set no leader wears hears no other mix"
    );
    assert!(
        !imported.mixes[&MixId::new("member1")].mixes[&MixId::new("member2")].muted,
        "a leader keeps what they hear"
    );
    let page = state.page("member2").unwrap();
    until("the pages free", || state.may_change(&page).is_ok()).await;
}

/// A person's mix moves only through the engine: without a synced one the
/// roster waits.
#[tokio::test]
async fn a_move_needs_a_synced_engine() {
    let dir = tempfile::tempdir().unwrap();
    let state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, running());
    store.save_state("youth", &stored).unwrap();
    let text = swapped();
    assert_eq!(
        apply_text(&state, &text, &sha256_hex(text.as_bytes())).await,
        Applied::Deferred(vec![
            "the engine state is not synced: the roster waits for it".to_string()
        ])
    );
    assert_eq!(store.state("youth").unwrap(), Some(stored), "put back");
    assert_eq!(state.roster(), Some(test_roster("main")));
}

/// A reload the engine does not answer waits: nothing changes, no status is
/// written, and the watcher reads the file again at its next poll.
#[tokio::test]
async fn a_reload_the_engine_does_not_answer_waits() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    let mut peer = synced(&mut state, running()).await;
    std::fs::write(d.join(ROSTER_FILE), swapped()).unwrap();
    let task = {
        let s = state.clone();
        tokio::spawn(async move { check_file(&s).await })
    };
    fake::next_import(&mut peer).await;
    // No answer: the request times out; the engine answers again, its
    // state as it was: the import did not go in.
    fake::answer_ping(&mut peer).await;
    let result = within(task).await;
    assert!(
        matches!(&result, Some(Applied::Deferred(p)) if p.len() == 1
            && p[0].ends_with("the engine did not answer in time")),
        "{result:?}"
    );
    assert!(
        !d.join(STATUS_FILE).exists(),
        "no status: it is tried again"
    );
    assert_eq!(state.roster(), Some(test_roster("main")));
    assert!(!state.frozen_now());
}

/// A reload writes the inactive lineups' stored states through the moves
/// and the lineup's rules.
#[tokio::test]
async fn an_inactive_lineups_stored_state_follows_the_roster() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let store = LineupStore::new(dir.path());
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, running());
    store.save_state("main", &stored).unwrap();
    // Youth's running state already fits youth (only the empty sets muted),
    // shaped as the engine holds it.
    let mut youth = shape::reconcile(&test_topology(), &running()).0;
    let lineup = test_roster("youth");
    fit_recorded(
        &mut youth,
        &lineup.roster,
        lineup.lineup(),
        &mut BTreeSet::new(),
    );
    let mut peer = synced(&mut state, youth).await;
    let text = moved();
    let applied_now = within(spawn_apply(&state, &text)).await;
    assert_eq!(applied_now, applied(&[], &[]));
    let main = store.state("main").unwrap().unwrap().state;
    let keys = |id: &str| main.mixes[&MixId::new(id)].inputs[&InputId::new("keys")].gain_db;
    assert_eq!(keys("member9"), -6.0, "member5's mix went along");
    assert!(
        main.mixes[&MixId::new("member5")].out.muted,
        "set member5 is empty"
    );
    assert_eq!(state.roster().unwrap().active, "youth");
    // Youth's sets did not change: no import was sent.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_legacy_server_adopts_its_first_roster() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    put(&d.join("presets").join("member1.json"), "member1's");
    let state = AppState::new(test_config(), d);
    let mut rx = state.lineup_tx.subscribe();
    state.announce_lineup();
    assert!(rx.try_recv().is_err(), "legacy mode announces nothing");
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    assert_eq!(check_file(&state).await, Some(applied(&[], &[])));
    let r = state.roster().expect("roster mode");
    assert_eq!(r.active, "main");
    assert_eq!(r.sha256, sha256_hex(TEXT.as_bytes()));
    let moved = d
        .join("lineups")
        .join("main")
        .join("presets")
        .join("member1.json");
    assert_eq!(read(&moved), "member1's");
    assert!(!d.join("presets").join("member1.json").exists());
    assert_eq!(
        rx.try_recv().unwrap(),
        LineupRef {
            id: "main".into(),
            name: "Main band".into(),
        }
    );
    assert!(status(d).ok);
    // A roster the migration refuses keeps legacy mode.
    let fresh = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(fresh.path().join("lineups")).unwrap();
    let legacy = AppState::new(test_config(), fresh.path());
    std::fs::write(fresh.path().join(ROSTER_FILE), TEXT).unwrap();
    assert!(matches!(
        check_file(&legacy).await,
        Some(Applied::Refused(p)) if p.len() == 1 && p[0].contains("exists without active.json")
    ));
    assert_eq!(legacy.roster(), None);
}

/// #58 review: a set the rules silenced because nobody wore it sounds
/// again once someone wears it.
#[tokio::test]
async fn a_set_the_rules_silenced_sounds_again_once_worn() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let mut peer = synced(&mut state, youth_running()).await;
    // member11 leaves: set member4 is silenced.
    let task = spawn_apply(&state, &without_member11());
    let gone = imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &["member11"]));
    assert!(gone.mixes[&MixId::new("member4")].out.muted);
    // member11 comes back: their set sounds again.
    let task = spawn_apply(&state, TEXT);
    let back = fake::imports(&mut peer, 3).await;
    assert_eq!(within(task).await, applied(&["member11"], &[]));
    assert!(!back.mixes[&MixId::new("member4")].out.muted);
}

/// #58 review: a roster change ends the solo on a set whose wearer it
/// changes (the engine keeps solos across an import), before the pages are
/// free; a set its wearer keeps keeps its solo.
#[tokio::test]
async fn a_reload_ends_the_solos_of_the_sets_that_change_wearer() {
    use iem_engine_proto::{EngineMsg, Reply, Solo, Source, Transient};
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let solo = |mix: &str, input: &str| Solo {
        mix: MixId::new(mix),
        sources: vec![Source::Input(InputId::new(input))],
    };
    let transient = Transient {
        solo: vec![solo("member5", "mic6"), solo("member1", "mic1")],
        ..Default::default()
    };
    peer.send(&EngineMsg::State {
        rev: 1,
        state: running(),
        transient: transient.clone(),
    })
    .await;
    until("synced", || state.engine.mirror().synced).await;
    // member5 and member6 swap sets.
    let task = spawn_apply(&state, &swapped());
    let (id, imported) = fake::next_import(&mut peer).await;
    peer.send(&EngineMsg::Reply(Reply {
        id,
        rev: 2,
        error: None,
    }))
    .await;
    peer.send(&EngineMsg::State {
        rev: 2,
        state: imported,
        transient,
    })
    .await;
    let (id, cmd) = fake::next_request(&mut peer).await;
    assert_eq!(
        cmd,
        Cmd::SetSolo {
            mix: MixId::new("member5"),
            sources: vec![],
        }
    );
    assert!(state.frozen_now(), "before the pages are free");
    peer.send(&EngineMsg::Reply(Reply {
        id,
        rev: 2,
        error: None,
    }))
    .await;
    assert_eq!(within(task).await, applied(&[], &[]));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "member1's solo stays"
    );
}

/// The record of what the rules silenced is the lineup's file: it
/// outlives the server.
#[tokio::test]
async fn the_record_of_what_the_rules_silenced_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let mut peer = synced(&mut state, youth_running()).await;
    let task = spawn_apply(&state, &without_member11());
    imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &["member11"]));
    let running = LineupStore::new(dir.path()).running().unwrap().unwrap();
    assert_eq!(
        (running.lineup.as_str(), running.silenced),
        ("youth", BTreeSet::from(["member4".to_string()]))
    );
}

/// #58 review: a set its wearer muted stays muted when it is left empty
/// and worn again: the rules unmute only what they silenced.
#[tokio::test]
async fn a_set_its_wearer_muted_stays_muted() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let mut live = youth_running();
    mix_mut(&mut live, "member4").out.muted = true;
    let mut peer = synced(&mut state, live).await;
    // Nothing changes in the running state: no import either time.
    assert_eq!(
        within(spawn_apply(&state, &without_member11())).await,
        applied(&[], &["member11"])
    );
    assert_eq!(
        within(spawn_apply(&state, TEXT)).await,
        applied(&["member11"], &[])
    );
    assert!(
        state.engine.mirror().state.mixes[&MixId::new("member4")]
            .out
            .muted
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err()
    );
}

/// Youth's running state as a switch leaves it: shaped by the topology, as
/// the engine holds every state, and youth's rules hold.
fn youth_running() -> MixState {
    let mut live = shape::reconcile(&test_topology(), &running()).0;
    let r = test_roster("youth");
    fit_recorded(&mut live, &r.roster, r.lineup(), &mut BTreeSet::new());
    live
}

/// #58 review: a reload tried while the engine is not synced waits, and
/// the watcher's next try once it is puts the roster's rules into the
/// running state and records the roster.
#[tokio::test]
async fn a_deferred_reload_brings_its_rules_once_the_engine_is_synced() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "youth");
    let text = without_member11();
    std::fs::write(d.join(ROSTER_FILE), &text).unwrap();
    assert!(matches!(
        check_file(&state).await,
        Some(Applied::Deferred(_))
    ));
    assert!(!d.join(STATUS_FILE).exists(), "tried again");
    let mut peer = synced(&mut state, youth_running()).await;
    let check = {
        let s = state.clone();
        tokio::spawn(async move { check_file(&s).await })
    };
    let imported = imports(&mut peer).await;
    assert_eq!(within(check).await, Some(applied(&[], &["member11"])));
    assert!(
        imported.mixes[&MixId::new("member4")].out.muted,
        "nobody wears set member4 in youth now"
    );
    assert_eq!(
        LineupStore::new(d).applied_roster().unwrap().as_deref(),
        Some(text.as_str())
    );
}

/// #58 review: a roster changed while the server was down moves the mixes
/// of the people it moves once the engine has synced (the roster the
/// states followed was recorded), and the pages hear of it.
#[tokio::test]
async fn the_start_moves_the_mixes_of_a_roster_changed_while_down() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    let text = moved();
    std::fs::write(d.join(ROSTER_FILE), &text).unwrap();
    let mut state = AppState::new(test_config(), d);
    let mut rx = state.lineup_tx.subscribe();
    let mut peer = synced(&mut state, running()).await;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    let imported = imports(&mut peer).await;
    within(settle).await;
    let keys = |id: &str| imported.mixes[&MixId::new(id)].inputs[&InputId::new("keys")].gain_db;
    assert_eq!(keys("member9"), -6.0, "member5's mix is on set member9");
    assert!(
        imported.mixes[&MixId::new("member5")].out.muted,
        "nobody wears set member5 now"
    );
    assert_eq!(rx.try_recv().unwrap().id, "main");
    assert_eq!(
        store.applied_roster().unwrap().as_deref(),
        Some(text.as_str())
    );
}

/// #58 review: a roster that cannot reach the states (a stored state that
/// cannot be read) does not keep the pages refused: after a few tries the
/// engineer's devices hear of it once and the pages are free.
#[tokio::test]
async fn a_settle_that_keeps_failing_alarms_and_frees_the_pages() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    std::fs::create_dir_all(store.data_dir("youth").join("state.json")).unwrap();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut state = AppState::new(test_config(), d);
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    let _peer = synced(&mut state, running()).await;
    state.retry = Duration::from_millis(100);
    let t0 = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(5), settle_when_synced(state.clone()))
        .await
        .expect("given up within 5 s");
    assert!(
        t0.elapsed() >= state.retry * (MAX_SETTLES - 1),
        "{MAX_SETTLES} tries, {:?} apart",
        state.retry
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "one alarm");
    let page = state.page("member2").unwrap();
    assert_eq!(state.may_change(&page), Ok(()));
    assert_eq!(
        store.applied_roster().unwrap(),
        None,
        "the states do not follow it"
    );
}

/// #58 review: no page changes a mix while a reload's import runs.
#[tokio::test]
async fn the_pages_are_frozen_during_a_reloads_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let mut peer = synced(&mut state, running()).await;
    let page = state.page("member1").unwrap();
    let task = spawn_apply(&state, &swapped());
    let (id, imported) = fake::next_import(&mut peer).await;
    assert_eq!(
        state.may_change(&page),
        Err(crate::view::ViewError::Switching)
    );
    fake::answer_import(&mut peer, id, 2, imported).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    assert_eq!(state.may_change(&page), Ok(()));
}

/// #58 review: a reload that comes while a switch runs waits for it, then
/// applies to the lineup the switch made active.
#[tokio::test]
async fn a_reload_waits_for_a_switch_and_applies_to_its_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let mut peer = synced(&mut state, running()).await;
    let switch = {
        let s = state.clone();
        let engineer = iem_core::AuthClaims {
            sub: "engineer".into(),
            engineer: true,
            exp: u64::MAX,
            iat: 0,
        };
        tokio::spawn(
            async move { crate::lineup_switch::switch_lineup(&s, "youth", &engineer).await },
        )
    };
    let (id, to_youth) = fake::next_import(&mut peer).await;
    let reload = spawn_apply(&state, &without_member11());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!reload.is_finished(), "the switch holds the lock");
    fake::answer_import(&mut peer, id, 2, to_youth.clone()).await;
    assert_eq!(within(switch).await, Ok(()));
    // Youth plays now: member11 leaves its set member4, silent after it.
    let imported = fake::imports(&mut peer, 3).await;
    assert_eq!(within(reload).await, applied(&[], &["member11"]));
    assert!(!to_youth.mixes[&MixId::new("member4")].out.muted);
    assert!(imported.mixes[&MixId::new("member4")].out.muted);
    assert_eq!(state.roster().unwrap().active, "youth");
}

/// #58 review: the watcher's step reads a roster that waits for the engine
/// (Deferred) again at every poll until it is in use; a stamp it has seen
/// is not read again, whatever the file holds.
#[tokio::test]
async fn the_watcher_reads_a_waiting_roster_again_at_every_poll() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "youth");
    let path = d.join(ROSTER_FILE);
    let text = without_member11();
    std::fs::write(&path, &text).unwrap();
    let first = stamp(&path);
    assert!(first.is_some());
    // The engine is not synced: the roster waits, and is read again.
    assert_eq!(poll(&state, &path, None).await, None);
    assert_eq!(poll(&state, &path, None).await, None);
    assert_eq!(state.roster(), Some(test_roster("youth")));
    let mut peer = synced(&mut state, youth_running()).await;
    let step = {
        let s = state.clone();
        let p = path.clone();
        tokio::spawn(async move { poll(&s, &p, None).await })
    };
    imports(&mut peer).await;
    assert_eq!(within(step).await, first, "in use: compared from now on");
    assert!(status(d).applied);
    let in_use = state.roster().unwrap().sha256;
    assert_eq!(in_use, sha256_hex(text.as_bytes()));
    std::fs::write(&path, TEXT).unwrap();
    let edited = stamp(&path);
    assert_eq!(poll(&state, &path, edited).await, edited);
    assert_eq!(state.roster().unwrap().sha256, in_use, "not read");
}

/// A settle tries again, uncounted, while the roster waits for the engine.
#[test]
fn only_a_refusal_counts_against_a_settle() {
    assert!(waits(&Applied::Deferred(vec![NOT_SYNCED.to_string()])));
    assert!(!waits(&Applied::Refused(Vec::new())));
}

/// #58 review: a set the rules silenced in an inactive lineup's stored
/// state sounds again there once someone wears it.
#[tokio::test]
async fn an_inactive_lineups_silenced_set_sounds_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    store
        .save_state("youth", &MixerBackup::new("t".into(), 1, youth_running()))
        .unwrap();
    let _peer = synced(&mut state, running()).await;
    let set4 = || {
        store.state("youth").unwrap().unwrap().state.mixes[&MixId::new("member4")]
            .out
            .muted
    };
    assert_eq!(
        within(spawn_apply(&state, &without_member11())).await,
        applied(&[], &["member11"])
    );
    assert!(set4(), "nobody wears it in youth");
    assert_eq!(
        within(spawn_apply(&state, TEXT)).await,
        applied(&["member11"], &[])
    );
    assert!(!set4(), "member11 wears it again");
}

/// #58 review: until the roster has reached the engine's state no switch
/// and no restore runs beside it.
#[tokio::test]
async fn nothing_switches_or_restores_before_the_roster_is_settled() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let (state, api) = crate::routes::api_tests::app(d);
    assert!(state.unsettled().is_some());
    let engineer = iem_core::AuthClaims {
        sub: "engineer".into(),
        engineer: true,
        exp: u64::MAX,
        iat: 0,
    };
    assert_eq!(
        crate::lineup_switch::switch_lineup(&state, "youth", &engineer).await,
        Err(crate::lineup_switch::SwitchError::Busy)
    );
    let b = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, MixState::default());
    let name = state.backup_store.save(&b).unwrap();
    let eng = crate::routes::api_tests::token("engineer", true);
    let (status, json) = crate::routes::api_tests::call(
        &api,
        axum::http::Method::POST,
        &format!("/api/backups/{name}/restore"),
        Some(&eng),
        None,
    )
    .await;
    assert_eq!(
        (status, json["code"].as_str()),
        (axum::http::StatusCode::CONFLICT, Some("SWITCHING")),
        "{json}"
    );
}

/// Connects `state` to a fake engine that can come back after a drop (its
/// next connections arrive on the receiver), its mirror holding `running`
/// at revision 1; retries 50 ms apart.
async fn reconnecting(
    state: &mut AppState,
    running: MixState,
) -> (fake::Peer, tokio::sync::mpsc::UnboundedReceiver<fake::Peer>) {
    let (connector, mut peers) = fake::connector();
    state.engine = crate::engine::EngineClient::spawn_with(connector, "test".into());
    state.retry = Duration::from_millis(50);
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 1, running).await;
    (peer, peers)
}

/// #58 re-review: the engine applied a reload's import and went away
/// before its answer: the pages stay frozen until it is back, and since
/// its state is the one sent the reload finishes, nobody moved twice.
#[tokio::test]
async fn a_reload_whose_import_went_in_before_the_engine_went_away_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let (mut peer, mut peers) = reconnecting(&mut state, running()).await;
    let text = swapped();
    let task = spawn_apply(&state, &text);
    let (_, sent) = fake::next_import(&mut peer).await;
    peer.send(&iem_engine_proto::EngineMsg::State {
        rev: 2,
        state: sent.clone(),
        transient: iem_engine_proto::Transient::default(),
    })
    .await;
    drop(peer);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(state.frozen_now(), "frozen until it is known");
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 2, sent.clone()).await;
    fake::answer_ping(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    assert_eq!(state.roster().unwrap().sha256, sha256_hex(text.as_bytes()));
    assert!(!state.frozen_now());
    assert_eq!(state.engine.mirror().state, sent, "moved once");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "no import again"
    );
}

/// #58 re-review: the engine went away before it applied a reload's
/// import: once it is back with its state as it was, the stored states go
/// back and the reload waits; the next try moves everyone once.
#[tokio::test]
async fn a_reload_whose_import_was_lost_with_the_engine_tries_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, running());
    store.save_state("youth", &stored).unwrap();
    let (mut peer, mut peers) = reconnecting(&mut state, running()).await;
    let task = spawn_apply(&state, &swapped());
    fake::next_import(&mut peer).await;
    drop(peer);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(state.frozen_now(), "frozen until it is known");
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 2, running()).await;
    fake::answer_ping(&mut peer).await;
    let result = within(task).await;
    assert!(matches!(&result, Applied::Deferred(_)), "{result:?}");
    assert_eq!(store.state("youth").unwrap(), Some(stored), "put back");
    assert_eq!(state.roster(), Some(test_roster("main")));
    assert!(!state.frozen_now());
    // The next try moves member5's and member6's mixes once.
    let task = spawn_apply(&state, &swapped());
    let imported = fake::imports(&mut peer, 3).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    let keys = |id: &str| {
        imported.mixes[&MixId::new(id)]
            .inputs
            .get(&InputId::new("keys"))
            .map(|l| l.gain_db)
    };
    assert_eq!(
        (keys("member6"), keys("member5")),
        (Some(-6.0), Some(Level::default().gain_db)),
        "swapped once"
    );
}

/// #58 re-review: a transition stopped after it wrote the stored states
/// (the server stopped or crashed during the import): the next start
/// moves nobody a second time.
#[tokio::test]
async fn a_start_after_a_cut_transition_moves_nobody_twice() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("youth", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    store
        .save_state("main", &MixerBackup::new("t".into(), 1, running()))
        .unwrap();
    // The new roster: member5 wears set member9 in main, member11 leaves
    // youth (so youth's running state changes too).
    let new = {
        let t = without_member11();
        let t = edit(&t, "member5 = \"member5\"\n", "");
        let t = edit(&t, "member9 = \"member9\"\n", "member9 = \"member5\"\n");
        edit(
            &t,
            "mic10 = { name = \"MEMBER9 mic\", owner = \"member9\" }\n",
            "mic10 = { name = \"MEMBER9 mic\" }\n",
        )
    };
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut first = AppState::new(test_config(), d);
    let mut peer = synced(&mut first, youth_running()).await;
    let task = spawn_apply(&first, &new);
    fake::next_import(&mut peer).await;
    // The server stops here: main's stored state is written, the import
    // never answered.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop((first, peer));
    std::fs::write(d.join(ROSTER_FILE), &new).unwrap();
    let mut state = AppState::new(test_config(), d);
    let mut peer = synced(&mut state, youth_running()).await;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    imports(&mut peer).await;
    within(settle).await;
    let main = store.state("main").unwrap().unwrap().state;
    let set9 = &main.mixes[&MixId::new("member9")];
    assert_eq!(set9.inputs[&InputId::new("keys")].gain_db, -6.0);
    assert!(
        !set9.out.muted,
        "moved once, not a copy of the set member5 left"
    );
}

/// [`moved`] without member11 (youth's set member4 is empty).
fn moved_without_member11() -> String {
    let t = without_member11();
    let t = edit(&t, "member5 = \"member5\"\n", "");
    let t = edit(&t, "member9 = \"member9\"\n", "member9 = \"member5\"\n");
    edit(
        &t,
        "mic10 = { name = \"MEMBER9 mic\", owner = \"member9\" }\n",
        "mic10 = { name = \"MEMBER9 mic\" }\n",
    )
}

/// #58 re-review: a settle that gives up frees the pages but keeps what
/// the states follow: the next roster change still moves the mixes of the
/// people a roster moved while the server was down.
#[tokio::test]
async fn a_settle_that_gave_up_still_moves_people_at_the_next_reload() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    let youth = store.data_dir("youth").join("state.json");
    std::fs::create_dir_all(&youth).unwrap();
    std::fs::write(d.join(ROSTER_FILE), moved()).unwrap();
    let mut state = AppState::new(test_config(), d);
    let mut peer = synced(&mut state, running()).await;
    state.retry = Duration::from_millis(20);
    within(tokio::spawn(settle_when_synced(state.clone()))).await;
    let page = state.page("member1").unwrap();
    assert_eq!(state.may_change(&page), Ok(()), "the pages are free");
    std::fs::remove_dir(&youth).unwrap();
    let task = spawn_apply(&state, &moved_without_member11());
    let imported = imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &["member11"]));
    let set9 = &imported.mixes[&MixId::new("member9")];
    assert_eq!(
        set9.inputs[&InputId::new("keys")].gain_db,
        -6.0,
        "member5's mix, moved now"
    );
}

/// #58 re-review: the settle at the start goes as soon as the engine's
/// state arrives, not at its next retry: the pages wait no longer.
#[tokio::test]
async fn the_settle_goes_once_the_engine_syncs() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    LineupStore::new(d).set_active("youth", 1).unwrap();
    let mut state = AppState::new(test_config(), d);
    state.retry = Duration::from_secs(10);
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake::sync(&state.engine, &mut peer, 1, running()).await;
    let t0 = std::time::Instant::now();
    imports(&mut peer).await;
    within(settle).await;
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
}

/// #58 re-review: a roster change waits while a switch a restart cut short
/// is not finished: until its replay has run, which band's state the
/// engine holds is not known.
#[tokio::test]
async fn a_reload_waits_for_an_unfinished_switch() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store
        .set_intent(&crate::lineup_store::SwitchIntent {
            from: "main".into(),
            to: "youth".into(),
        })
        .unwrap();
    let mut state = AppState::new(test_config(), d);
    assert!(state.frozen_now());
    let mut peer = synced(&mut state, running()).await;
    assert_eq!(
        within(spawn_apply(&state, &swapped())).await,
        Applied::Deferred(vec![
            "a band switch is not finished: the roster waits for it".to_string()
        ])
    );
    assert!(state.frozen_now(), "still the switch's");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "nothing sent"
    );
}

/// A site whose `roster.toml` the start cannot use (two main lineups)
/// beside the roster applied last (the test roster), main playing, the
/// engineer's device subscribed: the server starts with the copy.
async fn fallen_back(d: &Path) -> (AppState, crate::push::tests::Seen) {
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    let two = edit(
        TEXT,
        "name = \"Youth band\"\n",
        "name = \"Youth band\"\nmain = true\n",
    );
    std::fs::write(d.join(ROSTER_FILE), two).unwrap();
    let state = AppState::new(test_config(), d);
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    (state, seen)
}

/// #58 re-review: after a start on the roster applied last, the status
/// keeps saying so while `roster.toml` stays refused, and stops once it
/// can be used.
#[tokio::test]
async fn the_status_says_the_fallback_until_a_roster_applies() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (state, _) = fallen_back(d).await;
    let fallback = "in use instead: the roster applied last".to_string();
    assert_eq!(status(d).problems.last(), Some(&fallback));
    assert!(matches!(
        check_file(&state).await,
        Some(Applied::Refused(_))
    ));
    let s = status(d);
    assert_eq!(
        s.problems,
        [
            "exactly one lineup must be main, not 2".to_string(),
            fallback
        ]
    );
    // roster.toml is the roster in use again: in use, nothing refused.
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    check_file(&state).await;
    let s = status(d);
    assert!(s.ok && s.applied, "{s:?}");
    assert_eq!(s.sha256, sha256_hex(TEXT.as_bytes()));
}

/// #58 re-review: a start on the roster applied last tells the engineer's
/// devices once; a start on `roster.toml` does not.
#[tokio::test]
async fn a_fallback_start_alarms_the_engineer() {
    let dir = tempfile::tempdir().unwrap();
    let (state, seen) = fallen_back(dir.path()).await;
    assert_eq!(fallback_alarm(&state).await, 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    std::fs::write(dir.path().join(ROSTER_FILE), TEXT).unwrap();
    let state = AppState::new(test_config(), dir.path());
    assert_eq!(fallback_alarm(&state).await, 0, "roster.toml is in use");
}

/// #58 re-review: a settle records the roster in use as the one the states
/// follow only while `roster.toml` still is that roster: a file changed
/// since the start is the watcher's to apply and record.
#[tokio::test]
async fn a_settle_records_no_roster_file_changed_since_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut state = AppState::new(test_config(), d);
    std::fs::write(d.join(ROSTER_FILE), moved()).unwrap();
    let _peer = synced(&mut state, running()).await;
    within(tokio::spawn(settle_when_synced(state.clone()))).await;
    assert!(!state.settling());
    assert_eq!(LineupStore::new(d).applied_roster().unwrap(), None);
}

/// #58 third review: the engine holds an imported state shaped by its
/// topology (`iem_engine_proto::shape::reconcile`), so the server sends it
/// so shaped: a move from a set that hears peers to one that hears none
/// (member10 from set member1 to set member2 in youth) compares equal once
/// the import went in before the engine went away; the reload finishes and
/// nobody moves twice.
#[tokio::test]
async fn a_lost_import_from_a_peer_set_to_a_plain_set_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "youth");
    let (mut peer, mut peers) = reconnecting(&mut state, youth_running()).await;
    let text = swapped();
    let task = spawn_apply(&state, &text);
    let (_, sent) = fake::next_import(&mut peer).await;
    // The engine holds the import as its topology shapes it.
    let held = iem_engine_proto::shape::reconcile(&test_topology(), &sent).0;
    peer.send(&iem_engine_proto::EngineMsg::State {
        rev: 2,
        state: held.clone(),
        transient: iem_engine_proto::Transient::default(),
    })
    .await;
    drop(peer);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 2, held.clone()).await;
    fake::answer_ping(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    assert_eq!(state.roster().unwrap().sha256, sha256_hex(text.as_bytes()));
    assert!(!state.frozen_now());
    assert_eq!(sent, held, "sent as the engine holds it");
    assert!(
        held.mixes[&MixId::new("member2")].mixes.is_empty(),
        "set member2 hears no peers"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "no import again"
    );
}

/// #58 third review: when who wore the sets before is not known (no roster
/// the running state follows: a first roster, a legacy server's), a settle
/// ends every solo, the pages frozen.
#[tokio::test]
async fn a_settle_from_an_unknown_roster_ends_every_solo() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut state = AppState::new(test_config(), d);
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let solo = iem_engine_proto::Solo {
        mix: MixId::new("member3"),
        sources: vec![iem_engine_proto::Source::Input(InputId::new("mic3"))],
    };
    peer.send(&iem_engine_proto::EngineMsg::State {
        rev: 1,
        state: running(),
        transient: iem_engine_proto::Transient {
            solo: vec![solo],
            ..Default::default()
        },
    })
    .await;
    until("synced", || state.engine.mirror().synced).await;
    let settle = tokio::spawn(settle_when_synced(state.clone()));
    let (id, cmd) = fake::next_request(&mut peer).await;
    assert_eq!(
        cmd,
        Cmd::SetSolo {
            mix: MixId::new("member3"),
            sources: vec![],
        }
    );
    assert!(state.frozen_now());
    peer.send(&iem_engine_proto::EngineMsg::Reply(
        iem_engine_proto::Reply {
            id,
            rev: 1,
            error: None,
        },
    ))
    .await;
    within(settle).await;
    assert!(!state.frozen_now() && !state.settling());
}

/// #58 third review: after a switch a restart cut short, the start still
/// moves the mixes of the people a roster moved while the server was down:
/// the running state follows the roster applied last, as after any start.
#[tokio::test]
async fn a_start_after_a_cut_switch_still_moves_people() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    // A switch youth → main a restart cut short after main was recorded.
    store
        .set_intent(&crate::lineup_store::SwitchIntent {
            from: "youth".into(),
            to: "main".into(),
        })
        .unwrap();
    std::fs::write(d.join(ROSTER_FILE), moved()).unwrap();
    let mut state = AppState::new(test_config(), d);
    assert!(state.frozen_now());
    let mut peer = synced(&mut state, running()).await;
    let start = {
        let s = state.clone();
        tokio::spawn(async move {
            crate::lineup_switch::replay_when_synced(s.clone()).await;
            settle_when_synced(s).await;
        })
    };
    let imported = imports(&mut peer).await;
    within(start).await;
    let set9 = &imported.mixes[&MixId::new("member9")];
    assert_eq!(
        set9.inputs[&InputId::new("keys")].gain_db,
        -6.0,
        "member5's mix"
    );
}

/// #58 third review: every roster a state may follow is kept
/// (`lineups/rosters.kept/<sha>.toml`), so a stored state whose marker names a
/// roster this server applied before (here as a backup restored it) moves
/// from that roster: member10 to set member2 in youth.
#[tokio::test]
async fn a_state_on_a_kept_roster_moves_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    let kept = sha256_hex(TEXT.as_bytes());
    store.keep_roster(&kept, TEXT).unwrap();
    let mut youth = MixerBackup::new("t".into(), 1, running());
    youth.follows = Some(kept);
    store.save_state("youth", &youth).unwrap();
    let _peer = synced(&mut state, running()).await;
    let text = edit(TEXT, "member1 = \"member10\"\n", "member2 = \"member10\"\n");
    assert_eq!(within(spawn_apply(&state, &text)).await, applied(&[], &[]));
    let youth = store.state("youth").unwrap().unwrap();
    let set2 = &youth.state.mixes[&MixId::new("member2")];
    assert!(
        set2.mixes[&MixId::new("member1")].muted,
        "member10's mix, moved from set member1"
    );
    assert_eq!(youth.follows, Some(sha256_hex(text.as_bytes())));
}

/// #58 third review: two reloads one after the other, A to B, then B to C
/// where C moves member5 to set member9: the states the first marked B
/// move from B (the roster in use then) at the second.
#[tokio::test]
async fn a_second_reload_moves_people_from_the_roster_the_first_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let store = LineupStore::new(dir.path());
    store
        .save_state("youth", &MixerBackup::new("t".into(), 1, running()))
        .unwrap();
    let mut peer = synced(&mut state, running()).await;
    // A to B: member11 leaves youth, nothing of main changes (no import).
    let b = without_member11();
    let b_sha = sha256_hex(b.as_bytes());
    assert_eq!(
        within(spawn_apply(&state, &b)).await,
        applied(&[], &["member11"])
    );
    assert_eq!(
        store.running().unwrap().and_then(|r| r.follows),
        Some(b_sha.clone())
    );
    assert_eq!(
        store.state("youth").unwrap().unwrap().follows,
        Some(b_sha.clone())
    );
    // B to C: member5 wears set member9 in main.
    let c = edit(&b, "member5 = \"member5\"\n", "");
    let c = edit(&c, "member9 = \"member9\"\n", "member9 = \"member5\"\n");
    let c = edit(
        &c,
        "mic10 = { name = \"MEMBER9 mic\", owner = \"member9\" }\n",
        "mic10 = { name = \"MEMBER9 mic\" }\n",
    );
    let c_sha = sha256_hex(c.as_bytes());
    let task = spawn_apply(&state, &c);
    let imported = imports(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    let set9 = &imported.mixes[&MixId::new("member9")];
    assert_eq!(
        set9.inputs[&InputId::new("keys")].gain_db,
        -6.0,
        "member5's mix, moved from B's set member5"
    );
    assert_eq!(
        store.running().unwrap().and_then(|r| r.follows),
        Some(c_sha.clone())
    );
    assert_eq!(store.state("youth").unwrap().unwrap().follows, Some(c_sha));
}

/// #58 third review: while a reload's import whose answer was lost waits
/// for an engine that stays away, the engineer's devices hear of it once,
/// `AppState::absent_alarm` after the wait began; the wait goes on, and the
/// reload finishes once the engine is back with the import in.
#[tokio::test]
async fn a_lost_import_alarms_once_while_the_engine_stays_away() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = with_roster(dir.path(), "main");
    let (mut peer, mut peers) = reconnecting(&mut state, running()).await;
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    state.absent_alarm = Duration::from_millis(600);
    let alarms = || seen.lock().unwrap().len();
    let text = swapped();
    let task = spawn_apply(&state, &text);
    let (_, sent) = fake::next_import(&mut peer).await;
    drop(peer);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(alarms(), 0, "not before absent_alarm");
    let t0 = std::time::Instant::now();
    while alarms() == 0 {
        assert!(t0.elapsed() < Duration::from_secs(4), "no alarm");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(alarms(), 1, "one alarm");
    assert!(state.frozen_now(), "still waiting");
    assert!(!task.is_finished());
    let mut peer = fake::accept(&mut peers, test_topology()).await;
    fake::sync(&state.engine, &mut peer, 2, sent.clone()).await;
    fake::answer_ping(&mut peer).await;
    assert_eq!(within(task).await, applied(&[], &[]));
    assert_eq!(state.roster().unwrap().sha256, sha256_hex(text.as_bytes()));
    assert!(!state.frozen_now());
    assert_eq!(alarms(), 1);
}

#[test]
fn the_absent_engine_alarm_is_due_once_the_wait_reached_its_time() {
    let after = Duration::from_secs(60);
    assert!(!absent_alarm_due(false, Duration::from_secs(59), after));
    assert!(absent_alarm_due(false, after, after));
    assert!(absent_alarm_due(false, Duration::from_secs(61), after));
    assert!(
        !absent_alarm_due(true, Duration::from_secs(61), after),
        "once"
    );
}

/// #58 third review: the engine took a reload's import, but its answer was
/// lost with the server (it stopped, or the engine stayed away until a
/// reboot). `running.json` names the import pending with the digest of the
/// state sent, so the next start finds the engine holding that state and
/// moves nobody a second time (a swap made twice would undo itself).
#[tokio::test]
async fn a_start_after_an_import_that_went_in_unanswered_moves_nobody_twice() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut first = AppState::new(test_config(), d);
    let mut peer = synced(&mut first, running()).await;
    let new = swapped();
    let task = spawn_apply(&first, &new);
    let (_, sent) = fake::next_import(&mut peer).await;
    // The engine takes it; the server stops before the answer.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop((first, peer));
    assert_eq!(
        sent.mixes[&MixId::new("member6")].inputs[&InputId::new("keys")].gain_db,
        -6.0,
        "member5's mix on set member6"
    );
    std::fs::write(d.join(ROSTER_FILE), &new).unwrap();
    let mut state = AppState::new(test_config(), d);
    let mut peer = synced(&mut state, sent.clone()).await;
    within(tokio::spawn(settle_when_synced(state.clone()))).await;
    assert!(!state.settling());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), peer.recv())
            .await
            .is_err(),
        "no freeze and no import: nobody moves twice"
    );
    assert_eq!(state.engine.mirror().state, sent);
    let running = store.running().unwrap().unwrap();
    assert_eq!(running.follows, Some(sha256_hex(new.as_bytes())));
    assert_eq!(running.pending, None, "resolved");
}

/// #58 third review: a pending import counts once the engine holds the
/// state it sent (its digest), else the record before it stays; either way
/// the pending entry is gone.
#[test]
fn a_pending_import_counts_once_the_engine_holds_its_state() {
    let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    let sent = running();
    let before = Running {
        lineup: "main".into(),
        follows: Some("old".into()),
        silenced: set(&["member9"]),
        pending: Some(crate::lineup_store::Pending {
            follows: "new".into(),
            silenced: set(&["member5"]),
            digest: state_digest(&sent),
        }),
    };
    assert_eq!(
        resolved(before.clone(), &state_digest(&sent)),
        Running {
            lineup: "main".into(),
            follows: Some("new".into()),
            silenced: set(&["member5"]),
            pending: None,
        }
    );
    let other = state_digest(&MixState::default());
    assert_ne!(other, state_digest(&sent));
    assert_eq!(
        resolved(before.clone(), &other),
        Running {
            pending: None,
            ..before.clone()
        }
    );
    let plain = Running {
        pending: None,
        ..before
    };
    assert_eq!(resolved(plain.clone(), &other), plain);
}

/// #58 fourth review (decision): a roster learned from a state's marker
/// only moves people. The rules hold on the sets the roster before and the
/// new one list, never on a set only the learned roster lists. Here the
/// roster before and the new one list no set member5 (member5 wears set
/// member9 in main); youth's stored state follows the test roster, which
/// lists set member5 and puts member10 on set member1.
#[tokio::test]
async fn a_learned_roster_moves_people_but_brings_no_set_under_the_rules() {
    let dir = tempfile::tempdir().unwrap();
    let old = moved();
    let mut state = AppState::new(test_config(), dir.path());
    state.set_roster(Some(RosterState {
        roster: Arc::new(parse(&old)),
        sha256: sha256_hex(old.as_bytes()),
        active: "main".into(),
    }));
    let store = LineupStore::new(dir.path());
    let kept = sha256_hex(TEXT.as_bytes());
    store.keep_roster(&kept, TEXT).unwrap();
    let mut youth = MixerBackup::new("t".into(), 1, running());
    youth.follows = Some(kept);
    store.save_state("youth", &youth).unwrap();
    // Set member5 is silent in the running state already (its wearer's
    // mute), so main's running state takes no import either way.
    let mut live = running();
    mix_mut(&mut live, "member5").out.muted = true;
    let mut peer = synced(&mut state, live).await;
    // The new roster: member10 wears set member2 in youth.
    let new = edit(&old, "member1 = \"member10\"\n", "member2 = \"member10\"\n");
    assert_eq!(within(spawn_apply(&state, &new)).await, applied(&[], &[]));
    let youth = store.state("youth").unwrap().unwrap();
    assert!(
        youth.state.mixes[&MixId::new("member2")].mixes[&MixId::new("member1")].muted,
        "member10's mix, moved from the learned roster's set member1"
    );
    assert!(
        !youth.state.mixes[&MixId::new("member5")].out.muted,
        "set member5 is a set of neither the roster before nor the new one"
    );
    assert!(!youth.silenced.contains("member5"));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "no import"
    );
}

/// #58 fourth review: when what the running state follows once the import
/// is in (`Running::pending`) cannot be written before it, nothing is
/// imported (a lost answer and a stop would otherwise move the same people
/// again at the next start): the stored states go back, the pages are
/// free, and the roster waits for the next poll.
#[tokio::test]
async fn a_reload_whose_pending_record_cannot_be_written_imports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    let store = LineupStore::new(d);
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, running());
    store.save_state("youth", &stored).unwrap();
    // running.json cannot be written: a directory holds its name.
    std::fs::create_dir_all(d.join(crate::lineup_store::RUNNING_FILE)).unwrap();
    let mut peer = synced(&mut state, running()).await;
    let task = spawn_apply(&state, &swapped());
    // The freeze's barrier.
    fake::answer_ping(&mut peer).await;
    assert_eq!(
        within(task).await,
        Applied::Deferred(vec![
            "what the running state follows could not be written: the roster waits".to_string()
        ])
    );
    assert_eq!(store.state("youth").unwrap(), Some(stored), "put back");
    assert_eq!(state.roster(), Some(test_roster("main")));
    assert!(!state.frozen_now());
    assert!(
        tokio::time::timeout(Duration::from_millis(200), peer.recv())
            .await
            .is_err(),
        "no import"
    );
}

/// #58 fourth review: a settle whose running state follows the roster in
/// use already changes nobody's set, so a solo stays: no freeze, no
/// `SetSolo`, no import (a restart never ends the solos it finds).
#[tokio::test]
async fn a_settle_on_the_roster_the_running_state_follows_keeps_its_solos() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    store
        .set_running(&Running {
            lineup: "main".into(),
            follows: Some(sha256_hex(TEXT.as_bytes())),
            silenced: BTreeSet::new(),
            pending: None,
        })
        .unwrap();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let mut state = AppState::new(test_config(), d);
    let (engine, mut peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let solo = iem_engine_proto::Solo {
        mix: MixId::new("member5"),
        sources: vec![iem_engine_proto::Source::Input(InputId::new("mic6"))],
    };
    peer.send(&iem_engine_proto::EngineMsg::State {
        rev: 1,
        state: running(),
        transient: iem_engine_proto::Transient {
            solo: vec![solo],
            ..Default::default()
        },
    })
    .await;
    until("synced", || state.engine.mirror().synced).await;
    within(tokio::spawn(settle_when_synced(state.clone()))).await;
    assert!(!state.settling());
    assert!(!state.frozen_now());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), peer.recv())
            .await
            .is_err(),
        "no freeze, no SetSolo, no import"
    );
}

/// #58 fourth review: a pending import that a read of `running.json`
/// resolves (the start's settle or replay is the first) is saved at once,
/// taken or not, so it is never resolved again against an engine state
/// that pages have changed since.
#[test]
fn a_resolved_pending_import_is_saved_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = LineupStore::new(dir.path());
    let sent = running();
    let with_pending = Running {
        lineup: "main".into(),
        follows: Some("old".into()),
        silenced: BTreeSet::new(),
        pending: Some(Pending {
            follows: "new".into(),
            silenced: BTreeSet::from(["member5".to_string()]),
            digest: state_digest(&sent),
        }),
    };
    store.set_running(&with_pending).unwrap();
    let taken = running_of(&store, &state_digest(&sent)).unwrap();
    assert_eq!(taken.follows.as_deref(), Some("new"));
    assert_eq!(store.running().unwrap(), Some(taken), "saved, taken");
    store.set_running(&with_pending).unwrap();
    let not = running_of(&store, &state_digest(&MixState::default())).unwrap();
    assert_eq!(not.follows.as_deref(), Some("old"));
    assert_eq!(not.pending, None);
    assert_eq!(store.running().unwrap(), Some(not), "saved, not taken");
}

/// #58 fifth review: a settle that sees a synced engine resolves and saves
/// a pending import first, whatever fails after it. Here another lineup's
/// stored state cannot be read, which refuses the settle; the pending
/// import the engine holds must still be resolved (taken) and saved, so a
/// later switch or restore never resolves it against a state pages have
/// changed since.
#[tokio::test]
async fn a_refused_settle_still_resolves_a_pending_import() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    std::fs::write(d.join(ROSTER_FILE), TEXT).unwrap();
    let held = shape::reconcile(&test_topology(), &running()).0;
    let sha = sha256_hex(TEXT.as_bytes());
    store
        .set_running(&Running {
            lineup: "main".into(),
            follows: None,
            silenced: BTreeSet::new(),
            pending: Some(Pending {
                follows: sha.clone(),
                silenced: BTreeSet::new(),
                digest: state_digest(&held),
            }),
        })
        .unwrap();
    // Youth's stored state cannot be read: a directory holds its name.
    std::fs::create_dir_all(store.data_dir("youth").join("state.json")).unwrap();
    let mut state = AppState::new(test_config(), d);
    let _peer = synced(&mut state, held).await;
    assert!(matches!(settle(&state).await, Err(Applied::Refused(_))));
    let running = store.running().unwrap().unwrap();
    assert_eq!(running.follows, Some(sha), "the import had gone in");
    assert_eq!(running.pending, None, "resolved and saved");
}

/// #58 fifth review: a settle whose pending record cannot be written
/// (running.json unwritable) counts the try as refused: waiting does not
/// make the file writable, so the settle gives up after [`MAX_SETTLES`]
/// tries with one alarm and frees the pages.
#[tokio::test]
async fn a_settle_that_cannot_record_its_import_gives_up_with_one_alarm() {
    assert!(!waits(&Applied::Deferred(vec![NOT_RECORDED.to_string()])));
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let store = LineupStore::new(d);
    store.set_active("main", 1).unwrap();
    store.set_applied_roster(TEXT).unwrap();
    // member5 wears set member9 now: the running state takes an import.
    std::fs::write(d.join(ROSTER_FILE), moved()).unwrap();
    std::fs::create_dir_all(d.join(crate::lineup_store::RUNNING_FILE)).unwrap();
    let mut state = AppState::new(test_config(), d);
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    let mut peer = synced(&mut state, running()).await;
    state.retry = Duration::from_millis(100);
    // The engine answers every freeze's barrier; no import comes.
    let barriers = tokio::spawn(async move {
        loop {
            fake::answer_ping(&mut peer).await;
        }
    });
    tokio::time::timeout(Duration::from_secs(5), settle_when_synced(state.clone()))
        .await
        .expect("given up within 5 s");
    barriers.abort();
    assert_eq!(seen.lock().unwrap().len(), 1, "one alarm");
    let page = state.page("member2").unwrap();
    assert_eq!(state.may_change(&page), Ok(()));
}

/// #58 fifth review: a reload that keeps waiting because its pending record
/// cannot be written tells the engineer's devices once, after
/// [`MAX_SETTLES`] tries in a row, never at every poll.
#[tokio::test]
async fn a_reload_that_cannot_record_its_import_alarms_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    let (base, seen) = crate::push::tests::fake_push_service().await;
    state.config.write().await.vapid_private_key = crate::push::tests::vapid_private_key();
    state
        .push_store
        .write()
        .await
        .add(crate::push::tests::subscription(format!("{base}/201")))
        .unwrap();
    std::fs::create_dir_all(d.join(crate::lineup_store::RUNNING_FILE)).unwrap();
    let mut peer = synced(&mut state, running()).await;
    // The engine answers every freeze's barrier; no import comes.
    let barriers = tokio::spawn(async move {
        loop {
            fake::answer_ping(&mut peer).await;
        }
    });
    let alarms = || seen.lock().unwrap().len();
    let text = swapped();
    let unrecorded = Applied::Deferred(vec![NOT_RECORDED.to_string()]);
    for _ in 1..MAX_SETTLES {
        assert_eq!(within(spawn_apply(&state, &text)).await, unrecorded);
    }
    assert_eq!(alarms(), 0, "not before the last try");
    assert_eq!(within(spawn_apply(&state, &text)).await, unrecorded);
    assert_eq!(alarms(), 1, "one alarm, at the last try");
    assert_eq!(within(spawn_apply(&state, &text)).await, unrecorded);
    assert_eq!(alarms(), 1, "only one");
    barriers.abort();
}

/// #58 fifth review: a reload that imports nothing changes nothing for the
/// pages, so its thaw announces nothing and no person's mixer socket closes
/// (4003): a reload the watcher tries every 2 s would otherwise reconnect
/// every phone every 2 s.
#[tokio::test]
async fn a_reload_that_imports_nothing_closes_no_page() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    std::fs::create_dir_all(d.join(crate::lineup_store::RUNNING_FILE)).unwrap();
    let mut peer = synced(&mut state, running()).await;
    let mut rx = state.lineup_tx.subscribe();
    let task = spawn_apply(&state, &swapped());
    // The freeze's barrier.
    fake::answer_ping(&mut peer).await;
    assert_eq!(
        within(task).await,
        Applied::Deferred(vec![NOT_RECORDED.to_string()])
    );
    assert!(!state.frozen_now());
    assert!(rx.try_recv().is_err(), "nothing announced");
}

/// A roster path that exists but cannot be read as a file (here a
/// directory) is an error naming the path, never "no roster" (which would
/// mean legacy mode).
#[test]
fn a_roster_path_that_cannot_be_read_is_an_error_never_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_roster(dir.path()), Ok(None), "none there");
    let path = dir.path().join(ROSTER_FILE);
    std::fs::create_dir_all(&path).unwrap();
    let err = read_roster(dir.path()).unwrap_err();
    assert!(err.starts_with(&path.display().to_string()), "{err}");
}

/// What the running state follows is its own lineup's: asked for another
/// lineup, running_for answers no marker and no record.
#[test]
fn what_the_running_state_follows_is_only_its_own_lineups() {
    let dir = tempfile::tempdir().unwrap();
    let store = LineupStore::new(dir.path());
    let silenced = BTreeSet::from(["member4".to_string()]);
    store
        .set_running(&Running {
            lineup: "youth".into(),
            follows: Some("x".into()),
            silenced: silenced.clone(),
            pending: None,
        })
        .unwrap();
    assert_eq!(
        running_for(&store, "youth", ""),
        (Some("x".to_string()), silenced)
    );
    assert_eq!(running_for(&store, "main", ""), (None, BTreeSet::new()));
}

/// A stored state that cannot be written refuses a reload after the pages
/// were frozen for it: they are free again (they were free before), and
/// the stored states stay as they were.
#[tokio::test]
async fn a_stored_state_that_cannot_be_written_frees_the_pages_again() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut state = with_roster(d, "main");
    let store = LineupStore::new(d);
    let stored = MixerBackup::new("2026-10-21T12:00:00Z".into(), 1, running());
    store.save_state("youth", &stored).unwrap();
    // The write's temporary file cannot be made: a directory holds its name.
    std::fs::create_dir_all(store.data_dir("youth").join("state.tmp")).unwrap();
    let mut peer = synced(&mut state, running()).await;
    let task = spawn_apply(&state, &swapped());
    // The freeze's barrier: the running state changes, so the pages freeze.
    fake::answer_ping(&mut peer).await;
    let result = within(task).await;
    assert!(
        matches!(&result, Applied::Refused(p) if p.len() == 1
            && p[0].starts_with("the stored state of lineup youth was not written")),
        "{result:?}"
    );
    assert!(!state.frozen_now(), "free again");
    assert_eq!(store.state("youth").unwrap(), Some(stored));
    assert_eq!(state.roster(), Some(test_roster("main")));
}
