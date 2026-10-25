use super::*;

use iem_engine_proto::MixId;

use crate::AppState;
use crate::engine::client::fake;
use crate::site_view::tests::{departed_site, test_config, test_topology};

/// The test roster (`config/test-roster.toml`) with `active` playing.
pub(crate) fn test_roster(active: &str) -> RosterState {
    let roster = Roster::parse(include_str!("../../../../config/test-roster.toml"))
        .expect("config/test-roster.toml");
    RosterState {
        roster: Arc::new(roster),
        sha256: "test-roster".into(),
        active: active.into(),
    }
}

/// The test roster's text after member9 left (#58, the owner's look at the
/// live system): no person, set or input of theirs in any lineup, while the
/// site's legacy `[[members]]` and `[[inputs]]` still name member9, their
/// set member9 and their mic10.
pub(crate) fn departed_text() -> String {
    let cut = |text: &str, part: &str| {
        assert_eq!(text.matches(part).count(), 1, "{part}");
        text.replacen(part, "", 1)
    };
    let text = include_str!("../../../../config/test-roster.toml");
    let text = cut(
        text,
        "[[people]]\nid = \"member9\"\nname = \"Member 9\"\n\n",
    );
    let text = cut(&text, "member9 = \"member9\"\n");
    cut(
        &text,
        "mic10 = { name = \"MEMBER9 mic\", owner = \"member9\" }\n",
    )
}

/// [`departed_text`]'s roster with `active` playing.
pub(crate) fn departed_roster(active: &str) -> RosterState {
    let roster = Roster::parse(&departed_text()).expect("the roster without member9");
    assert_eq!(roster.problems(), Vec::<String>::new());
    RosterState {
        roster: Arc::new(roster),
        sha256: "departed-roster".into(),
        active: active.into(),
    }
}

/// The test site's people's equipment as a roster's `[equipment]` table
/// (#58, the last step of point 2): member1 ... member9's sets and their
/// inputs mic1 ... mic10 (mic5 is member4's guitar); `keys` is nobody's.
pub(crate) const EQUIPMENT: &str = "\n[equipment]\n\
    sets = [\"member1\", \"member2\", \"member3\", \"member4\", \"member5\", \"member6\", \"member7\", \"member8\", \"member9\"]\n\
    inputs = [\"mic1\", \"mic2\", \"mic3\", \"mic4\", \"mic5\", \"mic6\", \"mic7\", \"mic8\", \"mic9\", \"mic10\"]\n";

/// `text` (a roster) with [`EQUIPMENT`]: the roster declares the people's
/// equipment, so the site's legacy person entries are no longer read.
pub(crate) fn equipped_text(text: &str) -> String {
    format!("{text}{EQUIPMENT}")
}

/// [`departed_text`] with [`EQUIPMENT`] (member9 left; their set member9
/// and their mic10 are still equipment) and `active` playing.
pub(crate) fn equipped_departed(active: &str) -> RosterState {
    let roster = Roster::parse(&equipped_text(&departed_text()))
        .expect("the roster without member9, with [equipment]");
    assert_eq!(roster.problems(), Vec::<String>::new());
    RosterState {
        roster: Arc::new(roster),
        sha256: "equipped-departed-roster".into(),
        active: active.into(),
    }
}

fn ids(members: &[SiteMember]) -> Vec<(&str, &str, &str)> {
    members
        .iter()
        .map(|m| (m.id.as_str(), m.name.as_str(), m.mix.as_str()))
        .collect()
}

#[test]
fn the_active_lineup_and_its_leaders() {
    let youth = test_roster("youth");
    assert_eq!(youth.lineup().id, "youth");
    assert!(youth.is_leader("member3"));
    assert!(!youth.is_leader("member1"), "member1 leads main, not youth");
    let main = test_roster("main");
    assert_eq!(main.lineup().id, "main");
    assert!(main.is_leader("member1") && main.is_leader("member3"));
    assert!(!main.is_leader("member2"));
    // An unknown active lineup is never applied; should it be, main plays.
    assert_eq!(test_roster("gone").lineup().id, "main");
}

/// #58, the owner's look at the live system: a person's set is one a
/// lineup lists or one the site's members name, never the engineer's or
/// the translator's.
#[test]
fn person_sets_are_the_lineups_sets_and_the_site_members_sets() {
    let nine: BTreeSet<String> = (1..=9).map(|n| format!("member{n}")).collect();
    let cfg = test_config();
    assert_eq!(person_sets(&cfg, &test_roster("youth").roster), nine);
    assert_eq!(
        person_sets(&cfg, &departed_roster("main").roster),
        nine,
        "member9 left; the site's members still name their set"
    );
    let listed = person_sets(&Config::default(), &departed_roster("main").roster);
    assert_eq!(listed.len(), 8);
    assert!(!listed.contains("member9"), "no lineup lists it");
    let mut no_engineer_set = cfg.clone();
    no_engineer_set.members.retain(|m| m.id == ENGINEER_ID);
    assert_eq!(
        person_sets(&no_engineer_set, &test_roster("main").roster),
        nine,
        "the engineer's mix is no person's set"
    );
}

/// #58, owner option 1: an input is foreign to a lineup that does not list
/// it when it belongs to someone who does not play there (another lineup's
/// person, a departed member); the engineer's and the unowned ones never
/// are, and one the lineup lists is its own, owner or not.
#[test]
fn foreign_inputs_belong_to_people_who_do_not_play() {
    let cfg = test_config();
    let youth = test_roster("youth");
    let (r, l) = (&youth.roster, youth.lineup());
    let owners =
        |id: &str| -> Vec<String> { input_owners(&cfg, r, id).map(String::from).collect() };
    assert_eq!(
        owners("mic1"),
        ["member1", "member10"],
        "the lineups' owners only"
    );
    assert_eq!(owners("mic10"), ["member9"]);
    assert_eq!(owners("eng_mic"), ["engineer"]);
    assert!(owners("keys").is_empty());
    let foreign = |id: &str| foreign_input(&cfg, r, l, id);
    for id in ["mic2", "mic5", "mic6", "mic10"] {
        assert!(foreign(id), "{id}");
    }
    for id in ["mic1", "mic3", "mic4", "eng_mic", "keys", "hand1", "drums"] {
        assert!(!foreign(id), "{id}");
    }
    // member9 left: the site's member9 is no roster person.
    let gone = departed_roster("main");
    assert!(foreign_input(&cfg, &gone.roster, gone.lineup(), "mic10"));
    assert!(!foreign_input(&cfg, &gone.roster, gone.lineup(), "mic9"));
    // Youth names main's member4's guitar input for itself, without an owner.
    let mut listed = test_roster("youth");
    Arc::make_mut(&mut listed.roster)
        .lineups
        .iter_mut()
        .find(|l| l.id == "youth")
        .unwrap()
        .inputs
        .insert(
            "mic5".into(),
            iem_core::roster::LineupInput {
                name: "SHARED gtr".into(),
                owner: None,
            },
        );
    assert!(!foreign_input(
        &cfg,
        &listed.roster,
        listed.lineup(),
        "mic5"
    ));
}

/// #58 review: the site's `[[inputs]]` owner counts only when no lineup
/// gives the input an owner. A site owner who does not play (here
/// member2, while youth plays) never hides an input a lineup gives to
/// someone who plays (main's member3 owns mic3); with no lineup owner the
/// site's owner decides, as for a departed person's mic10.
#[test]
fn the_site_owner_counts_only_when_no_lineup_owns_the_input() {
    let mut cfg = test_config();
    for m in cfg.inputs.iter_mut() {
        if m.id == "mic3" || m.id == "keys" {
            m.owner = Some("member2".into());
        }
    }
    let youth = test_roster("youth");
    let (r, l) = (&youth.roster, youth.lineup());
    let owners =
        |id: &str| -> Vec<String> { input_owners(&cfg, r, id).map(String::from).collect() };
    assert_eq!(owners("mic3"), ["member3"], "a lineup owns it");
    assert_eq!(owners("keys"), ["member2"], "no lineup owns it");
    assert!(
        !foreign_input(&cfg, r, l, "mic3"),
        "member3 plays in youth; the site's member2 does not count"
    );
    assert!(
        foreign_input(&cfg, r, l, "keys"),
        "the site's owner decides"
    );
    let gone = departed_roster("youth");
    assert!(foreign_input(&cfg, &gone.roster, gone.lineup(), "mic10"));
}

/// #58, the last step of point 2: with `[equipment]` a person's set is
/// exactly one it lists. The site's legacy `[[members]]` are not read: a
/// departed person's set stays one on a site without their entry, and a
/// site member's set the equipment leaves out is none.
#[test]
fn with_equipment_the_person_sets_are_its_sets() {
    let nine: BTreeSet<String> = (1..=9).map(|n| format!("member{n}")).collect();
    let gone = equipped_departed("main");
    assert_eq!(person_sets(&departed_site(), &gone.roster), nine);
    assert_eq!(person_sets(&Config::default(), &gone.roster), nine);
    // Without [equipment] (today's rule) that site leaves member9 out.
    assert!(!person_sets(&departed_site(), &departed_roster("main").roster).contains("member9"));
    // A set the site's members name and the equipment does not is none.
    let mut fewer = equipped_departed("main");
    Arc::make_mut(&mut fewer.roster)
        .equipment
        .as_mut()
        .unwrap()
        .sets
        .retain(|s| s != "member9");
    assert_eq!(fewer.roster.problems(), Vec::<String>::new());
    let eight = person_sets(&test_config(), &fewer.roster);
    assert_eq!(eight.len(), 8);
    assert!(!eight.contains("member9"), "the site's member9 is not read");
}

/// #58, the last step of point 2: with `[equipment]` an input is foreign to
/// the active lineup exactly when it is listed there and the lineup does
/// not list it. The site's `[[inputs]]` owners are not read: the departed
/// member9's mic10 stays hidden without its site owner, and a site owner of
/// an input the equipment does not list hides nothing. So a lineup lists
/// every person's input it shows: youth, which does not list member3's
/// mic3, hides it while it plays; once youth lists it, it shows.
#[test]
fn with_equipment_an_input_is_foreign_when_listed_and_not_the_lineups() {
    let site = departed_site();
    let foreign =
        |cfg: &Config, r: &RosterState, id: &str| foreign_input(cfg, &r.roster, r.lineup(), id);
    let youth = equipped_departed("youth");
    for id in [
        "mic2", "mic3", "mic5", "mic6", "mic7", "mic8", "mic9", "mic10",
    ] {
        assert!(foreign(&site, &youth, id), "youth: {id}");
    }
    for id in ["mic1", "mic4", "eng_mic", "keys", "hand1", "drums"] {
        assert!(!foreign(&site, &youth, id), "youth: {id}");
    }
    let main = equipped_departed("main");
    assert!(foreign(&site, &main, "mic10"));
    for id in ["mic1", "mic5", "mic9", "eng_mic", "keys"] {
        assert!(!foreign(&site, &main, id), "main: {id}");
    }
    // The site's owners are not read: mic10's, nor one given to keys.
    let mut owned = test_config();
    for m in owned.inputs.iter_mut() {
        if m.id == "keys" {
            m.owner = Some("member2".into());
        }
    }
    assert!(
        !foreign(&owned, &youth, "keys"),
        "keys is no person's input"
    );
    let owners = |cfg: &Config, id: &str| -> Vec<String> {
        input_owners(cfg, &youth.roster, id)
            .map(String::from)
            .collect()
    };
    assert!(owners(&owned, "keys").is_empty());
    assert!(
        owners(&test_config(), "mic10").is_empty(),
        "no lineup owns mic10"
    );
    assert_eq!(owners(&owned, "mic1"), ["member1", "member10"]);
    // Youth lists member3's mic3: it shows while youth plays.
    let mut listed = equipped_departed("youth");
    Arc::make_mut(&mut listed.roster)
        .lineups
        .iter_mut()
        .find(|l| l.id == "youth")
        .unwrap()
        .inputs
        .insert(
            "mic3".into(),
            iem_core::roster::LineupInput {
                name: "MEMBER3 mic".into(),
                owner: Some("member3".into()),
            },
        );
    assert_eq!(listed.roster.problems(), Vec::<String>::new());
    assert!(!foreign(&site, &listed, "mic3"));
}

#[test]
fn site_members_are_the_lineups_people_on_their_sets_then_the_engineer() {
    let cfg = test_config();
    assert_eq!(site_members(&cfg, None), cfg.members, "legacy: the site's");
    assert_eq!(
        ids(&site_members(&cfg, Some(&test_roster("youth")))),
        [
            ("member10", "Member 10", "member1"),
            ("member3", "Member 3", "member3"),
            ("member11", "Member 11", "member4"),
            ("engineer", "Engineer", "engineer"),
        ]
    );
    let main = site_members(&cfg, Some(&test_roster("main")));
    assert_eq!(main.len(), 10);
    assert_eq!(ids(&main)[1], ("member2", "Member 2", "member2"));
    // A site without its engineer entry has no engineer member.
    let bare = Config::default();
    assert_eq!(site_members(&bare, Some(&test_roster("youth"))).len(), 3);
}

#[test]
fn people_are_every_login_tile_playing_first() {
    let cfg = test_config();
    let tiles = |r: Option<&RosterState>| -> Vec<(String, bool)> {
        people(&cfg, r)
            .into_iter()
            .map(|p| (p.id, p.playing))
            .collect()
    };
    let legacy = tiles(None);
    assert_eq!(legacy.len(), 10);
    assert!(legacy.iter().all(|(_, playing)| *playing));
    assert_eq!(legacy[0].0, "member1");
    assert_eq!(legacy[9].0, "engineer");
    let youth = tiles(Some(&test_roster("youth")));
    let want: Vec<(String, bool)> = [
        ("member10", true),
        ("member3", true),
        ("member11", true),
        ("member1", false),
        ("member2", false),
        ("member4", false),
        ("member5", false),
        ("member6", false),
        ("member7", false),
        ("member8", false),
        ("member9", false),
        ("engineer", true),
    ]
    .into_iter()
    .map(|(id, playing)| (id.to_string(), playing))
    .collect();
    assert_eq!(youth, want);
    let named = people(&cfg, Some(&test_roster("youth")));
    assert_eq!(
        named[3],
        PersonView {
            id: "member1".into(),
            name: "Member 1".into(),
            playing: false,
        }
    );
    assert_eq!(named[11].name, "Engineer");
    let main = tiles(Some(&test_roster("main")));
    assert_eq!(main.len(), 12);
    assert_eq!(main[8], ("member9".to_string(), true));
    assert_eq!(main[9], ("member10".to_string(), false));
}

/// Without an engine the pages come from the site, or the active lineup.
#[tokio::test]
async fn pages_before_the_engine_follow_the_active_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(test_config(), dir.path());
    assert_eq!(state.page("member1").unwrap().mix, MixId::new("member1"));
    assert!(state.page("member10").is_none(), "legacy: no such member");
    *state.roster.write().unwrap() = Some(test_roster("youth"));
    let p = state.page("member10").unwrap();
    assert_eq!(
        (
            p.id.as_str(),
            p.name.as_str(),
            p.mix.0.as_str(),
            p.member.as_deref()
        ),
        ("member10", "Member 10", "member1", Some("member10"))
    );
    assert!(
        state.page("member1").is_none(),
        "member1 plays in main only"
    );
    assert_eq!(state.page("engineer").unwrap().mix, MixId::new("engineer"));
    assert_eq!(state.roster(), Some(test_roster("youth")));
}

/// The site view is rebuilt when the roster or its active lineup changes,
/// and kept while nothing changed.
#[tokio::test]
async fn the_site_view_follows_the_roster_and_its_active_lineup() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(test_config(), dir.path());
    let (engine, _peer) = fake::announced(test_topology()).await;
    state.engine = engine;
    let legacy = state.site().unwrap();
    assert!(Arc::ptr_eq(&legacy, &state.site().unwrap()), "kept");
    assert_eq!(legacy.member("member1").unwrap().name, "Member1");
    assert!(state.page("member10").is_none());

    *state.roster.write().unwrap() = Some(test_roster("main"));
    let main = state.site().unwrap();
    assert_eq!(main.member("member1").unwrap().name, "Member 1");
    assert!(Arc::ptr_eq(&main, &state.site().unwrap()), "kept");

    state.roster.write().unwrap().as_mut().unwrap().active = "youth".into();
    let youth = state.site().unwrap();
    assert_eq!(
        youth.member("member10").map(|m| m.mix.as_str()),
        Some("member1")
    );
    assert_eq!(state.page("member10").unwrap().mix, MixId::new("member1"));
    assert!(
        state.page("member2").is_none(),
        "nobody wears member2's set"
    );

    // A new roster file with the same active lineup: built again.
    state.roster.write().unwrap().as_mut().unwrap().sha256 = "edited".into();
    assert!(!Arc::ptr_eq(&youth, &state.site().unwrap()));
}
