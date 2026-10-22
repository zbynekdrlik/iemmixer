use super::*;

use iem_engine_proto::MixId;

use crate::AppState;
use crate::engine::client::fake;
use crate::site_view::tests::{test_config, test_topology};

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
