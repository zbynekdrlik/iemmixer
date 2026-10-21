use super::*;

use iem_engine_proto::{InputId, InputInfo, MixId, MixInfo};

const ROSTER: &str = include_str!("../../../../config/test-roster.toml");
const SITE: &str = include_str!("../../../../config/test-site.toml");

fn roster() -> Roster {
    Roster::parse(ROSTER).expect("config/test-roster.toml")
}

fn facts() -> TopologyFacts {
    site_facts(SITE).expect("the test site's [engine] table")
}

fn person(id: &str) -> Person {
    Person {
        id: id.into(),
        name: format!("Name of {id}"),
        takes_over: None,
    }
}

#[test]
fn the_test_roster_parses_and_fits_the_test_site() {
    let r = roster();
    assert_eq!(r.problems(), Vec::<String>::new());
    assert_eq!(r.problems_against(&facts()), Vec::<String>::new());
    assert_eq!(r.people.len(), 11);
    assert_eq!(
        r.person("member10").map(|p| p.name.as_str()),
        Some("Member 10")
    );
    assert!(r.person("engineer").is_none());
    assert_eq!(r.main().id, "main");
    assert_eq!(r.lineups.len(), 2);
    let main = r.lineup("main").unwrap();
    assert_eq!(main.leaders, ["member1", "member3"]);
    assert_eq!(main.sets.len(), 9);
    assert_eq!(main.inputs.len(), 10);
    let youth = r.lineup("youth").unwrap();
    assert!(!youth.main);
    assert_eq!(youth.name, "Youth band");
    assert_eq!(youth.leaders, ["member3"]);
    assert_eq!(
        youth.inputs["mic1"],
        LineupInput {
            name: "MEMBER10 mic".into(),
            owner: Some("member10".into())
        }
    );
    assert!(r.lineup("nobody").is_none());
    // Who wears what.
    assert_eq!(r.set_of("youth", "member10"), Some("member1"));
    assert_eq!(r.set_of("youth", "member2"), None);
    assert_eq!(r.set_of("main", "member2"), Some("member2"));
    assert_eq!(r.set_of("nobody", "member2"), None);
    assert_eq!(youth.person_on("member4"), Some("member11"));
    assert_eq!(youth.person_on("member2"), None);
    assert!(youth.plays("member3") && youth.plays("member11"));
    assert!(!youth.plays("member1"));
    assert!(youth.leads("member3") && !youth.leads("member10"));
    assert!(main.leads("member1") && !main.leads("member2"));
    // A leader of any lineup.
    assert!(r.leads_any("member1") && r.leads_any("member3"));
    assert!(!r.leads_any("member10") && !r.leads_any("engineer"));
}

#[test]
fn site_facts_read_the_engine_table() {
    let f = facts();
    assert_eq!(f.engineer, "engineer");
    assert_eq!(f.inputs.len(), 24);
    assert_eq!(f.inputs[0], "mic1");
    assert_eq!(f.mixes.len(), 11);
    let mix = |id: &str| f.mixes.iter().find(|m| m.id == id).unwrap();
    assert!(mix("member1").stereo && mix("engineer").stereo);
    assert!(!mix("translator").stereo);
    assert_eq!(mix("member1").hears.len(), 8);
    assert_eq!(mix("member1").hears[0], "member2");
    assert_eq!(mix("member3").hears[0], "member1");
    assert_eq!(mix("engineer").hears.len(), 9);
    assert!(mix("member2").hears.is_empty());
    // The mixes it hears whole, then its peers.
    let text = "[engine]\nengineer = \"e\"\n[[engine.mixes]]\nid = \"a\"\ntx = [1, 2]\nmixes = [\"b\"]\npeers = [\"c\"]\n";
    let f = site_facts(text).unwrap();
    assert_eq!(f.mixes[0].hears, ["b", "c"]);
    assert!(f.inputs.is_empty());
    assert_eq!(
        site_facts("port = 80\n"),
        Err(RosterError::Site("no [engine] table".into()))
    );
    assert!(matches!(
        site_facts("port = \n"),
        Err(RosterError::Site(e)) if e.starts_with("line 1: ")
    ));
}

#[test]
fn facts_of_an_announced_topology() {
    let topo = TopologyInfo {
        hash: "h".into(),
        sample_rate: 96_000,
        engineer: MixId::new("eng"),
        inputs: vec![InputInfo {
            id: InputId::new("mic1"),
            channels: 1,
            talkback: false,
            group: None,
        }],
        groups: vec![],
        mixes: vec![
            MixInfo {
                id: MixId::new("a"),
                channels: 2,
                mixes: vec![MixId::new("b")],
                peers: vec![MixId::new("c")],
            },
            MixInfo {
                id: MixId::new("t"),
                channels: 1,
                mixes: vec![],
                peers: vec![],
            },
        ],
    };
    assert_eq!(
        TopologyFacts::from(&topo),
        TopologyFacts {
            mixes: vec![
                MixFact {
                    id: "a".into(),
                    stereo: true,
                    hears: vec!["b".into(), "c".into()],
                },
                MixFact {
                    id: "t".into(),
                    stereo: false,
                    hears: vec![],
                },
            ],
            inputs: vec!["mic1".into()],
            engineer: "eng".into(),
        }
    );
}

#[test]
fn lineup_ids_name_a_directory() {
    let longest = "a".repeat(MAX_LINEUP_ID);
    for ok in ["main", "youth", "a", "band_2-x", "7", longest.as_str()] {
        assert!(valid_lineup_id(ok), "{ok}");
    }
    let too_long = "a".repeat(MAX_LINEUP_ID + 1);
    for bad in [
        "",
        "Main",
        "../x",
        "a b",
        "a.b",
        "b\u{e1}nd",
        too_long.as_str(),
    ] {
        assert!(!valid_lineup_id(bad), "{bad}");
    }
}

type Edit = fn(&mut Roster);

#[test]
fn problems_name_every_offender() {
    let base = roster();
    assert_eq!(base.problems(), Vec::<String>::new());
    let cases: [(&str, Edit); 22] = [
        (
            "person: member_id '../x' contains invalid characters",
            |r| r.people[1].id = "../x".into(),
        ),
        ("person id 'Member2' must be lower case", |r| {
            r.people[1].id = "Member2".into()
        }),
        (
            "person id 'engineer' is the engineer's, never a person",
            |r| r.people.push(person("engineer")),
        ),
        ("person 'member2' is listed twice", |r| {
            r.people.push(person("member2"))
        }),
        ("person 'member2' has no name", |r| {
            r.people[1].name = " ".into()
        }),
        (
            "person 'member10': takes_over 'member10' must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("member10".into()),
        ),
        (
            "person 'member10': takes_over 'Old1' must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("Old1".into()),
        ),
        (
            "person 'member10': takes_over '../x' must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("../x".into()),
        ),
        (
            "person 'member10': takes_over 'engineer' must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("engineer".into()),
        ),
        (
            "person 'member10': takes_over 'member2' is a person, whose files are their own",
            |r| r.people[9].takes_over = Some("member2".into()),
        ),
        (
            "takes_over 'old1' is claimed by both 'member10' and 'member11'",
            |r| {
                r.people[9].takes_over = Some("old1".into());
                r.people[10].takes_over = Some("old1".into());
            },
        ),
        ("lineup id 'Youth' must be 1 to 32 of a-z 0-9 _ -", |r| {
            r.lineups[1].id = "Youth".into()
        }),
        ("lineup id '' must be 1 to 32 of a-z 0-9 _ -", |r| {
            r.lineups[1].id = String::new()
        }),
        ("lineup 'main' is listed twice", |r| {
            let mut copy = r.lineups[0].clone();
            copy.main = false;
            r.lineups.push(copy);
        }),
        ("lineup 'youth' has no name", |r| {
            r.lineups[1].name = "".into()
        }),
        (
            "lineup 'youth': set 'member2': 'ghost' is not a person",
            |r| {
                r.lineups[1].sets.insert("member2".into(), "ghost".into());
            },
        ),
        ("lineup 'youth': person 'member3' wears two sets", |r| {
            r.lineups[1].sets.insert("member2".into(), "member3".into());
        }),
        ("lineup 'youth': leader 'member1' wears no set of it", |r| {
            r.lineups[1].leaders.push("member1".into())
        }),
        ("lineup 'youth': input 'mic2' has no name", |r| {
            r.lineups[1].inputs.insert(
                "mic2".into(),
                LineupInput {
                    name: " ".into(),
                    owner: None,
                },
            );
        }),
        (
            "lineup 'youth': input 'mic1': owner 'member1' wears no set of it",
            |r| r.lineups[1].inputs.get_mut("mic1").unwrap().owner = Some("member1".into()),
        ),
        ("exactly one lineup must be main, not 2", |r| {
            r.lineups[1].main = true
        }),
        ("exactly one lineup must be main, not 0", |r| {
            r.lineups[0].main = false
        }),
    ];
    for (want, edit) in cases {
        let mut r = base.clone();
        edit(&mut r);
        let p = r.problems();
        assert!(p.iter().any(|x| x.contains(want)), "{want}: {p:?}");
    }
    // A person who takes over a former member's files, and a lineup input
    // without an owner, are fine.
    let mut r = base.clone();
    r.people[9].takes_over = Some("old1".into());
    r.lineups[1].inputs.insert(
        "keys".into(),
        LineupInput {
            name: "KEYS".into(),
            owner: None,
        },
    );
    assert_eq!(r.problems(), Vec::<String>::new());
}

#[test]
fn exactly_one_main_lineup() {
    let mut r = roster();
    r.lineups[1].main = true;
    assert_eq!(r.problems(), ["exactly one lineup must be main, not 2"]);
    r.lineups[0].main = false;
    r.lineups[1].main = false;
    assert_eq!(r.problems(), ["exactly one lineup must be main, not 0"]);
    r.lineups[1].main = true;
    assert_eq!(r.problems(), Vec::<String>::new());
    assert_eq!(r.main().id, "youth");
}

#[test]
fn a_leader_whose_set_cannot_hear_the_others_is_refused() {
    // member1's set no longer hears member3's (a site copy without that peer).
    // The engineer's `mixes` lists the same run of ids, so match `peers = [`.
    let from = "peers = [\"member2\", \"member3\", \"member4\"";
    assert_eq!(SITE.matches(from).count(), 1, "only member1's peers");
    let site = SITE.replacen(from, "peers = [\"member2\", \"member4\"", 1);
    let facts = site_facts(&site).unwrap();
    assert_eq!(
        roster().problems_against(&facts),
        ["leader member1 in main: set member1 does not hear member3; the site needs peers (F30)"]
    );
    // A leader without a set is problems()'s to name, not this check's.
    let mut r = roster();
    r.lineups[1].leaders.push("member2".into());
    assert_eq!(r.problems_against(&self::facts()), Vec::<String>::new());
}

#[test]
fn sets_and_inputs_must_be_the_sites() {
    let mut r = roster();
    let youth = &mut r.lineups[1];
    youth.sets.insert("translator".into(), "member5".into());
    youth.sets.insert("engineer".into(), "member6".into());
    youth.sets.insert("ghost".into(), "member7".into());
    youth.inputs.insert(
        "nowhere".into(),
        LineupInput {
            name: "X".into(),
            owner: None,
        },
    );
    let not_a_set = |s: &str| {
        format!(
            "lineup 'youth': set '{s}' is not a stereo mix of the site other than the engineer's"
        )
    };
    let unheard = |s: &str| {
        format!(
            "leader member3 in youth: set member3 does not hear {s}; the site needs peers (F30)"
        )
    };
    assert_eq!(
        r.problems_against(&facts()),
        [
            not_a_set("engineer"),
            not_a_set("ghost"),
            not_a_set("translator"),
            "lineup 'youth': input 'nowhere' is not an input of the site".to_string(),
            unheard("engineer"),
            unheard("ghost"),
            unheard("translator"),
        ]
    );
}

#[test]
fn unknown_keys_are_refused() {
    for (from, to) in [
        // Top level, a person, a lineup and a lineup's input.
        (
            "[[people]]\nid = \"member1\"",
            "pin = \"2468\"\n[[people]]\nid = \"member1\"",
        ),
        (
            "name = \"Member 1\"\n",
            "name = \"Member 1\"\npin = \"2468\"\n",
        ),
        ("main = true\n", "main = true\ncolour = \"red\"\n"),
        (
            "mic2 = { name = \"MEMBER2 mic\", owner = \"member2\" }",
            "mic2 = { name = \"MEMBER2 mic\", owner = \"member2\", category = \"mics\" }",
        ),
    ] {
        assert_eq!(ROSTER.matches(from).count(), 1, "{from}");
        let text = ROSTER.replacen(from, to, 1);
        match Roster::parse(&text) {
            Err(RosterError::Parse(e)) => assert!(e.contains("unknown field"), "{e}"),
            other => panic!("{to}: {other:?}"),
        }
    }
    // A lineup needs its sets.
    let text = ROSTER.replacen(
        "[lineups.sets]\nmember1 = \"member10\"",
        "[lineups.x]\nmember1 = \"member10\"",
        1,
    );
    assert!(matches!(Roster::parse(&text), Err(RosterError::Parse(_))));
}

#[test]
fn a_parse_error_names_the_line_never_the_value() {
    let bad = "main = \"zyxname\"\n";
    let text = ROSTER.replacen("main = true\n", bad, 1);
    let line = text[..text.find(bad).unwrap()].matches('\n').count() + 1;
    let Err(RosterError::Parse(e)) = Roster::parse(&text) else {
        panic!("a string where a boolean belongs");
    };
    assert!(e.starts_with(&format!("line {line}: ")), "{e}");
    assert!(
        e.contains("invalid type") && e.contains("\"\u{2026}\""),
        "{e}"
    );
    assert!(!e.contains("zyxname"), "{e}");
    assert!(
        RosterError::Parse(e.clone())
            .to_string()
            .starts_with("roster file: line ")
    );
    // A syntax error: its line too.
    let Err(RosterError::Parse(e)) = Roster::parse("[[people]]\nid = \"a\"\nname = = \"b\"\n")
    else {
        panic!("a syntax error");
    };
    assert!(e.starts_with("line 3: "), "{e}");
}
