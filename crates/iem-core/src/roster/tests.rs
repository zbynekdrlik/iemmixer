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

/// #58 review: the server's start reads the facts from the site config's
/// own `[engine]` table, the same as `roster check` reads from the file.
#[test]
fn engine_facts_read_the_site_configs_table() {
    let config: crate::config::Config = toml::from_str(SITE).unwrap();
    assert_eq!(engine_facts(config.engine.as_ref().unwrap()), Ok(facts()));
    let table: toml::Table = toml::from_str("inputs = []\n").unwrap();
    assert!(matches!(
        engine_facts(&table),
        Err(RosterError::Site(e)) if e.starts_with("[engine]: ")
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
    let cases: [(&str, Edit); 23] = [
        (
            "person #2: the id must be lower-case a-z 0-9 _ -, at least one",
            |r| r.people[1].id = "../x".into(),
        ),
        (
            "person #2: the id must be lower-case a-z 0-9 _ -, at least one",
            |r| r.people[1].id = "Member2".into(),
        ),
        (
            "person 'engineer' is the engineer's id, never a person's",
            |r| r.people.push(person("engineer")),
        ),
        ("person 'member2' is listed twice", |r| {
            r.people.push(person("member2"))
        }),
        ("person 'member2' has no name", |r| {
            r.people[1].name = " ".into()
        }),
        (
            "person 'member10': takes_over must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("member10".into()),
        ),
        (
            "person 'member10': takes_over must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("Old1".into()),
        ),
        (
            "person 'member10': takes_over must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("../x".into()),
        ),
        (
            "person 'member10': takes_over must be another member's lower-case id",
            |r| r.people[9].takes_over = Some("engineer".into()),
        ),
        (
            "takes_over 'old1' is claimed by both person 'member10' and person 'member11'",
            |r| {
                r.people[9].takes_over = Some("old1".into());
                r.people[10].takes_over = Some("old1".into());
            },
        ),
        ("lineup #2: the id must be 1 to 32 of a-z 0-9 _ -", |r| {
            r.lineups[1].id = "Youth".into()
        }),
        ("lineup #2: the id must be 1 to 32 of a-z 0-9 _ -", |r| {
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
        (
            "lineup 'youth': set 'member3': 'member3' wears another set too",
            |r| {
                r.lineups[1].sets.insert("member2".into(), "member3".into());
            },
        ),
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
        // A person's id named by position, a lineup's too.
        ("person #1 has no name", |r| {
            r.people[0].id = "Member1".into();
            r.people[0].name = String::new();
        }),
        ("lineup #2 has no name", |r| {
            r.lineups[1].id = "Youth".into();
            r.lineups[1].name = String::new();
        }),
    ];
    for (want, edit) in cases {
        let mut r = base.clone();
        edit(&mut r);
        let p = r.problems();
        assert!(p.iter().any(|x| x.contains(want)), "{want}: {p:?}");
    }
    // A person who takes over a former member's files (or, the migration
    // long done, a person's: `takes_over` is read only by the migration),
    // and a lineup input without an owner, are fine.
    let mut r = base.clone();
    r.people[9].takes_over = Some("old1".into());
    r.people[10].takes_over = Some("member2".into());
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

/// #58 review: a valid but wrong roster would remove everyone (every
/// person it lacks is deleted), so a roster with no people, or whose main
/// lineup places nobody on a set, is a problem; another lineup may be
/// empty.
#[test]
fn a_roster_without_people_or_a_main_without_sets_is_refused() {
    let empty = Roster::parse(
        "people = []\n\n[[lineups]]\nid = \"main\"\nname = \"Main\"\nmain = true\n\n[lineups.sets]\n",
    )
    .unwrap();
    assert_eq!(
        empty.problems(),
        [
            "the roster has no people",
            "lineup 'main' is main and has no sets"
        ]
    );
    // The test roster's main lineup with nobody on its sets (no leaders
    // and no owners either, which would be problems of their own).
    let clear = |l: &mut Lineup| {
        l.sets.clear();
        l.leaders.clear();
        for input in l.inputs.values_mut() {
            input.owner = None;
        }
    };
    let mut r = roster();
    clear(&mut r.lineups[0]);
    assert_eq!(r.problems(), ["lineup 'main' is main and has no sets"]);
    // A lineup that is not main may be empty.
    let mut r = roster();
    clear(&mut r.lineups[1]);
    assert_eq!(r.problems(), Vec::<String>::new());
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

#[test]
fn quoted_spans_are_cut_to_their_quotes() {
    let dots = "\u{2026}";
    assert_eq!(redact("no quotes"), "no quotes");
    assert_eq!(
        redact("invalid type: string \"x\\\"zyxname\", expected a boolean"),
        format!("invalid type: string \"{dots}\", expected a boolean")
    );
    assert_eq!(
        redact("unknown field `zyxname`, expected `id` or `name`"),
        format!("unknown field `{dots}`, expected `{dots}` or `{dots}`")
    );
    // A backslash escapes the next character, a doubled one too; an
    // unclosed span runs to the end.
    assert_eq!(
        redact("a \"b\\\\\" c `d\\`e` f"),
        format!("a \"{dots}\" c `{dots}` f")
    );
    assert_eq!(redact("a \"b"), format!("a \"{dots}\""));
    assert_eq!(redact("`\"x` \"`y\""), format!("`{dots}` \"{dots}\""));
}

/// P6: a name typed into a key or a value never comes back in the error.
#[test]
fn a_parse_error_drops_a_quoted_key_and_an_escaped_value() {
    for (from, to) in [
        // A quoted key in a table that refuses unknown keys.
        (
            "name = \"Member 1\"\n",
            "name = \"Member 1\"\n\"zyxname\" = 1\n",
        ),
        // A value holding an escaped quote.
        ("main = true\n", "main = \"x\\\"zyxname\"\n"),
    ] {
        assert_eq!(ROSTER.matches(from).count(), 1, "{from}");
        let text = ROSTER.replacen(from, to, 1);
        let Err(RosterError::Parse(e)) = Roster::parse(&text) else {
            panic!("{to}: refused");
        };
        assert!(!e.to_lowercase().contains("zyxname"), "{e}");
        assert!(e.starts_with("line "), "{e}");
    }
}

/// P6: a value that is no valid id (a name typed into an id field) is named
/// by its field and position, in problems() and problems_against() alike.
#[test]
fn problems_never_echo_a_value_that_is_no_id() {
    // Three typed names: a person's id, a wearer/key/lineup id placed in a
    // set, and a leader/owner placed nowhere.
    let (id, typed, nowhere) = ("Wvqz Yx", "Zyxq Wvn", "Qzyx Lw");
    let mut r = roster();
    r.people[1].id = id.into();
    r.people[9].takes_over = Some(typed.into());
    r.lineups[1].id = typed.into();
    r.lineups[1].sets.insert("member4".into(), typed.into());
    r.lineups[1].leaders.push(typed.into());
    let main = &mut r.lineups[0];
    main.sets.insert(typed.into(), "member1".into());
    main.sets.insert("member9".into(), typed.into());
    main.leaders.push(nowhere.into());
    main.inputs.insert(
        typed.into(),
        LineupInput {
            name: String::new(),
            owner: Some(nowhere.into()),
        },
    );
    let p = r.problems();
    let against = r.problems_against(&facts());
    for x in p.iter().chain(&against) {
        let x = x.to_lowercase();
        for name in ["wvqz", "zyxq", "qzyx"] {
            assert!(!x.contains(name), "{x}");
        }
    }
    let has = |list: &[String], s: &str| list.iter().any(|x| x == s);
    for want in [
        "person #2: the id must be lower-case a-z 0-9 _ -, at least one",
        "person 'member10': takes_over must be another member's lower-case id",
        "lineup #2: the id must be 1 to 32 of a-z 0-9 _ -",
        "lineup 'main': set 'member1': 'member1' wears another set too",
        "lineup 'main': set 'member9': its wearer is not a person",
        "lineup 'main': leader #3 wears no set of it",
        "lineup 'main': input #1 has no name",
        "lineup 'main': input #1: owner its owner wears no set of it",
        "lineup #2: set 'member4': its wearer is not a person",
    ] {
        assert!(has(&p, want), "{want}: {p:?}");
    }
    for want in [
        "lineup 'main': set #1 is not a stereo mix of the site other than the engineer's",
        "lineup 'main': input #1 is not an input of the site",
        // member1 also wears the typed set, which `set_of` finds first (keys
        // sort upper case first), so member3's line is the stable one.
        "leader member3 in main: set member3 does not hear #1; the site needs peers (F30)",
        "leader #2 in lineup #2: set member4 does not hear member1; the site needs peers (F30)",
    ] {
        assert!(has(&against, want), "{want}: {against:?}");
    }
}

/// P6: a leader's own set that is no valid id is named by its position
/// among the lineup's sets (keys in order), here the third.
#[test]
fn a_leaders_set_that_is_no_id_is_named_by_its_position() {
    let mut r = roster();
    let main = &mut r.lineups[0];
    main.sets.remove("member3");
    // Sorts after member1 and member2: the third set.
    main.sets.insert("member3 x".into(), "member3".into());
    let against = r.problems_against(&facts());
    let want = "leader member3 in main: set #3 does not hear member1; the site needs peers (F30)";
    assert!(against.iter().any(|x| x == want), "{against:?}");
    assert!(
        !against.iter().any(|x| x.contains("member3 x")),
        "{against:?}"
    );
}

/// The test site's people's equipment (#58): member1 ... member9's sets and
/// their inputs mic1 ... mic10 (mic5 is member4's guitar), as a roster's
/// `[equipment]` table.
const EQUIPMENT: &str = "\n[equipment]\n\
    sets = [\"member1\", \"member2\", \"member3\", \"member4\", \"member5\", \"member6\", \"member7\", \"member8\", \"member9\"]\n\
    inputs = [\"mic1\", \"mic2\", \"mic3\", \"mic4\", \"mic5\", \"mic6\", \"mic7\", \"mic8\", \"mic9\", \"mic10\"]\n";

/// The test roster with [`EQUIPMENT`].
fn equipped() -> Roster {
    Roster::parse(&format!("{ROSTER}{EQUIPMENT}")).expect("the test roster with [equipment]")
}

fn ids(range: std::ops::RangeInclusive<u32>, prefix: &str) -> Vec<String> {
    range.map(|n| format!("{prefix}{n}")).collect()
}

/// #58, the last step of point 2: the roster may declare the people's
/// equipment; without the table there is none, as before.
#[test]
fn the_equipment_table_is_read_and_fits_the_test_site() {
    assert_eq!(roster().equipment, None);
    let r = equipped();
    assert_eq!(
        r.equipment,
        Some(Equipment {
            sets: ids(1..=9, "member"),
            inputs: ids(1..=10, "mic"),
        })
    );
    assert_eq!(r.problems(), Vec::<String>::new());
    assert_eq!(r.problems_against(&facts()), Vec::<String>::new());
    // Both lists are required, and nothing else is read.
    for (from, to) in [
        ("inputs = [", "parts = ["),
        ("sets = [", "kit = ["),
        ("[equipment]\n", "[equipment]\nmembers = []\n"),
    ] {
        let text = format!("{ROSTER}{EQUIPMENT}").replacen(from, to, 1);
        assert!(
            matches!(Roster::parse(&text), Err(RosterError::Parse(_))),
            "{to}"
        );
    }
}

/// #58: with `[equipment]` every set a lineup places someone on must be one
/// of its sets, and every input a lineup gives an owner one of its inputs;
/// its own ids are valid and listed once (a value that is no id named by
/// position, P6). An input a lineup lists without an owner need not be.
#[test]
fn equipment_problems_name_every_offender() {
    type EquipmentEdit = fn(&mut Equipment);
    let cases: [(&[&str], EquipmentEdit); 6] = [
        (
            &[
                "lineup 'main': set 'member4' is not in [equipment] sets",
                "lineup 'youth': set 'member4' is not in [equipment] sets",
            ],
            |e| e.sets.retain(|s| s != "member4"),
        ),
        (
            &["lineup 'main': input 'mic5' has an owner and is not in [equipment] inputs"],
            |e| e.inputs.retain(|i| i != "mic5"),
        ),
        (
            &[
                "lineup 'main': input 'mic1' has an owner and is not in [equipment] inputs",
                "lineup 'youth': input 'mic1' has an owner and is not in [equipment] inputs",
            ],
            |e| e.inputs.retain(|i| i != "mic1"),
        ),
        (&["[equipment]: set #10 is no valid id"], |e| {
            e.sets.push("Zyxq Wvn".into())
        }),
        (&["[equipment]: set 'member2' is listed twice"], |e| {
            e.sets.push("member2".into())
        }),
        (
            &[
                "[equipment]: input #11 is no valid id",
                "[equipment]: input 'mic3' is listed twice",
            ],
            |e| {
                e.inputs.push("Qzyx Lw".into());
                e.inputs.push("mic3".into());
            },
        ),
    ];
    for (want, edit) in cases {
        let mut r = equipped();
        edit(r.equipment.as_mut().unwrap());
        let p = r.problems();
        assert_eq!(p, want, "{want:?}");
        for x in &p {
            assert!(!x.to_lowercase().contains("zyxq"), "{x}");
            assert!(!x.to_lowercase().contains("qzyx"), "{x}");
        }
    }
    // A lineup input without an owner (youth's keys) is no person's.
    let mut r = equipped();
    r.lineups[1].inputs.insert(
        "keys".into(),
        LineupInput {
            name: "KEYS".into(),
            owner: None,
        },
    );
    assert_eq!(r.problems(), Vec::<String>::new());
    // Without [equipment] none of these is checked.
    let mut r = roster();
    r.lineups[1].sets.insert("member2".into(), "member2".into());
    assert_eq!(r.problems(), Vec::<String>::new());
}

/// #58: each set of `[equipment]` is a stereo mix of the site other than
/// the engineer's (the translator's mono mix is no person's set), each input
/// one of the site's inputs; a value that is no id is named by position.
#[test]
fn the_equipment_must_be_the_sites() {
    let mut r = equipped();
    let e = r.equipment.as_mut().unwrap();
    e.sets
        .extend(["translator", "engineer", "ghost", "Zyxq Wvn"].map(String::from));
    e.inputs.extend(["nowhere", "Qzyx Lw"].map(String::from));
    let not_a_set = |s: &str| {
        format!("[equipment]: set {s} is not a stereo mix of the site other than the engineer's")
    };
    let not_an_input = |s: &str| format!("[equipment]: input {s} is not an input of the site");
    assert_eq!(
        r.problems_against(&facts()),
        [
            not_a_set("'translator'"),
            not_a_set("'engineer'"),
            not_a_set("'ghost'"),
            not_a_set("#13"),
            not_an_input("'nowhere'"),
            not_an_input("#12"),
        ]
    );
}
