//! The site as the UI sees it (S5 design note §3, §6): the configured members
//! and input labels joined with the engine's topology. Built again whenever
//! the engine announces a topology or the roster or its active lineup
//! changes; ids the topology does not have are reported and left out, never
//! guessed. In roster mode (#58) the members are the active lineup's people
//! on their sets and the lineup names its inputs (`roster_state`).

use iem_core::config::{Config, SiteMember};
use iem_engine_proto::{GroupId, InputId, MixId, TopologyInfo};

use crate::pin_store::ENGINEER_ID;
use crate::roster_state::{self, PersonView, RosterState};

/// One engine input with its label, tab and owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputView {
    pub id: InputId,
    pub name: String,
    /// "mics", "stems" or "tech".
    pub category: String,
    pub owner: Option<String>,
    pub group: Option<GroupId>,
}

/// One engine mix with the member who hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixView {
    pub id: MixId,
    pub name: String,
    pub member: Option<String>,
    /// The mixes it hears (its Mixes tab) in its level slots' order: the
    /// mixes it hears whole, then its peers (#58).
    pub hears: Vec<MixId>,
    pub mono: bool,
}

/// A mixer page: a member's (`/member3`) or, for the engineer, a mix
/// without a member (`/translator`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: String,
    pub name: String,
    pub mix: MixId,
    pub member: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteView {
    pub members: Vec<SiteMember>,
    pub inputs: Vec<InputView>,
    pub mixes: Vec<MixView>,
    /// The group shown as the STEMS strip (the site's first group).
    pub group: Option<GroupId>,
    pub engineer_mix: Option<MixId>,
    /// Who hears the other members' mixes: the active lineup's leaders;
    /// legacy, every member (not the engineer) whose mix hears another.
    pub leaders: Vec<String>,
    /// Every login tile, each with whether they play (legacy: the members,
    /// all playing; `roster_state::people`).
    pub people: Vec<PersonView>,
    /// People's sets nobody wears in the active lineup
    /// (`roster_state::person_sets`: a departed person's too): no page, no
    /// Mixes channel (legacy: none).
    pub vacant: Vec<MixId>,
}

/// "translator" → "Translator".
fn title(id: &str) -> String {
    let mut c = id.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl SiteView {
    /// The view of `config` over `topo`, and every mismatch between them
    /// (legacy mode).
    pub fn build(config: &Config, topo: &TopologyInfo) -> (Self, Vec<String>) {
        Self::build_with(config, None, topo)
    }

    /// The view of `config` and, in roster mode, the roster's active lineup
    /// over `topo`, and every mismatch between them. An input the lineup
    /// lists takes its name and owner; any other keeps the site's label and
    /// has no owner but the engineer's own channel (#58 design §5).
    pub fn build_with(
        config: &Config,
        roster: Option<&RosterState>,
        topo: &TopologyInfo,
    ) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let lineup = roster.map(RosterState::lineup);
        let mix_ids: Vec<&MixId> = topo.mixes.iter().map(|m| &m.id).collect();
        let members: Vec<SiteMember> = roster_state::site_members(config, roster)
            .into_iter()
            .filter(|m| {
                let known = mix_ids.iter().any(|id| id.0 == m.mix);
                if !known {
                    problems.push(format!(
                        "member '{}': the engine has no mix '{}'",
                        m.id, m.mix
                    ));
                }
                known
            })
            .collect();
        let inputs = topo
            .inputs
            .iter()
            .map(|i| {
                let meta = config.inputs.iter().find(|m| m.id == i.id.0);
                if meta.is_none() {
                    problems.push(format!("input '{}' has no [[inputs]] label", i.id));
                }
                let category = if i.group.is_some() {
                    "stems".to_string()
                } else {
                    meta.and_then(|m| m.category.clone())
                        .unwrap_or_else(|| "mics".to_string())
                };
                let listed = lineup.and_then(|l| l.inputs.get(&i.id.0));
                let site_owner = meta.and_then(|m| m.owner.clone());
                let owner = match (lineup, listed) {
                    (None, _) => site_owner,
                    (Some(_), Some(l)) => l.owner.clone(),
                    (Some(_), None) => site_owner.filter(|o| o == ENGINEER_ID),
                };
                InputView {
                    id: i.id.clone(),
                    name: match (listed, meta) {
                        (Some(l), _) => l.name.clone(),
                        (None, Some(m)) => m.name.clone(),
                        (None, None) => i.id.0.to_uppercase(),
                    },
                    category,
                    owner,
                    group: i.group.clone(),
                }
            })
            .collect();
        for m in &config.inputs {
            if !topo.inputs.iter().any(|i| i.id.0 == m.id) {
                problems.push(format!("[[inputs]] '{}' is not an engine input", m.id));
            }
        }
        if let Some(l) = lineup {
            for id in l.inputs.keys() {
                if !topo.inputs.iter().any(|i| i.id.0 == *id) {
                    problems.push(format!(
                        "lineup '{}': input '{id}' is not an engine input",
                        l.id
                    ));
                }
            }
        }
        let mixes = topo
            .mixes
            .iter()
            .map(|m| {
                let member = members.iter().find(|s| s.mix == m.id.0);
                MixView {
                    id: m.id.clone(),
                    name: member.map_or_else(|| title(&m.id.0), |s| s.name.clone()),
                    member: member.map(|s| s.id.clone()),
                    hears: m.mixes.iter().chain(&m.peers).cloned().collect(),
                    mono: m.channels == 1,
                }
            })
            .collect();
        let leaders = match lineup {
            Some(l) => l.leaders.clone(),
            None => members
                .iter()
                .filter(|m| {
                    m.id != ENGINEER_ID
                        && topo.mixes.iter().any(|x| {
                            x.id.0 == m.mix && (!x.mixes.is_empty() || !x.peers.is_empty())
                        })
                })
                .map(|m| m.id.clone())
                .collect(),
        };
        let vacant = match (roster, lineup) {
            (Some(r), Some(l)) => {
                let sets = roster_state::person_sets(config, &r.roster);
                topo.mixes
                    .iter()
                    .filter(|m| l.person_on(&m.id.0).is_none() && sets.contains(&m.id.0))
                    .map(|m| m.id.clone())
                    .collect()
            }
            _ => Vec::new(),
        };
        let view = Self {
            members,
            inputs,
            mixes,
            group: topo.groups.first().map(|g| g.id.clone()),
            engineer_mix: Some(topo.engineer.clone()),
            leaders,
            people: roster_state::people(config, roster),
            vacant,
        };
        (view, problems)
    }

    pub fn member(&self, id: &str) -> Option<&SiteMember> {
        self.members.iter().find(|m| m.id == id)
    }

    pub fn mix(&self, id: &MixId) -> Option<&MixView> {
        self.mixes.iter().find(|m| &m.id == id)
    }

    pub fn input(&self, id: &str) -> Option<&InputView> {
        self.inputs.iter().find(|i| i.id.0 == id)
    }

    /// The page `id`: a member, or a mix without a member that is no vacant
    /// set.
    pub fn page(&self, id: &str) -> Option<Page> {
        if let Some(m) = self.member(id) {
            return Some(Page {
                id: m.id.clone(),
                name: m.name.clone(),
                mix: MixId::new(m.mix.clone()),
                member: Some(m.id.clone()),
            });
        }
        self.mixes
            .iter()
            .find(|m| m.id.0 == id && m.member.is_none() && !self.vacant.contains(&m.id))
            .map(|m| Page {
                id: m.id.0.clone(),
                name: m.name.clone(),
                mix: m.id.clone(),
                member: None,
            })
    }

    /// Pages without a member (engineer only, F29).
    pub fn mix_pages(&self) -> Vec<Page> {
        self.mixes
            .iter()
            .filter(|m| m.member.is_none())
            .filter_map(|m| self.page(&m.id.0))
            .collect()
    }

    /// The member whose page `mix` is.
    pub fn member_of_mix(&self, mix: &MixId) -> Option<&SiteMember> {
        self.members.iter().find(|m| m.mix == mix.0)
    }

    /// Display name of a mix (a member's name, else the capitalised id).
    pub fn mix_name(&self, mix: &MixId) -> String {
        self.mix(mix)
            .map_or_else(|| title(&mix.0), |m| m.name.clone())
    }

    /// The member's own channel: their first owned input (F5 "more me").
    pub fn own_input(&self, member: &str) -> Option<&InputId> {
        self.inputs
            .iter()
            .find(|i| i.owner.as_deref() == Some(member))
            .map(|i| &i.id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The test site's topology, compiled by the engine itself.
    pub(crate) fn test_topology() -> TopologyInfo {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/test-site.toml");
        let site = iem_engine::site::load(&path).expect("the test site's [engine] table");
        iem_engine::topology::compile(&site)
            .expect("the test site compiles")
            .info()
    }

    pub(crate) fn test_config() -> Config {
        toml::from_str(include_str!("../../../config/test-site.toml")).expect("test site")
    }

    pub(crate) fn test_view() -> SiteView {
        let (view, problems) = SiteView::build(&test_config(), &test_topology());
        assert_eq!(problems, Vec::<String>::new());
        view
    }

    #[test]
    fn the_test_site_joins_its_topology_without_problems() {
        let v = test_view();
        assert_eq!(v.members.len(), 10);
        assert_eq!(v.inputs.len(), 24);
        assert_eq!(v.mixes.len(), 11);
        assert_eq!(v.group, Some(GroupId::new("stems")));
        assert_eq!(v.engineer_mix, Some(MixId::new("engineer")));
        let mic5 = v.input("mic5").unwrap();
        assert_eq!(
            (
                mic5.name.as_str(),
                mic5.category.as_str(),
                mic5.owner.as_deref()
            ),
            ("MEMBER4 gtr", "mics", Some("member4"))
        );
        assert_eq!(v.input("drums").unwrap().category, "stems");
        assert_eq!(v.input("iemonly").unwrap().category, "stems");
        assert_eq!(v.input("content").unwrap().category, "tech");
        assert_eq!(v.input("keys").unwrap().category, "mics");
        assert_eq!(v.own_input("member4"), Some(&InputId::new("mic4")));
        assert_eq!(v.own_input("member9"), Some(&InputId::new("mic10")));
        assert_eq!(v.own_input("nobody"), None);
        let translator = v.mix(&MixId::new("translator")).unwrap();
        assert_eq!(
            (
                translator.name.as_str(),
                translator.member.as_deref(),
                translator.mono
            ),
            ("Translator", None, true)
        );
        assert_eq!(v.mix(&MixId::new("member1")).unwrap().hears.len(), 8);
        assert_eq!(v.mix(&MixId::new("engineer")).unwrap().hears.len(), 9);
        assert_eq!(v.mix_name(&MixId::new("member3")), "Member3");
        assert_eq!(v.mix_name(&MixId::new("gone")), "Gone");
        assert_eq!(
            v.member_of_mix(&MixId::new("engineer"))
                .map(|m| m.id.as_str()),
            Some("engineer")
        );
    }

    /// #58: a peer is a mix the page hears, after the mixes it hears whole.
    #[test]
    fn a_mix_hears_its_peers_after_its_mixes() {
        let v = test_view();
        let m3 = v.mix(&MixId::new("member3")).unwrap();
        assert_eq!(m3.hears.len(), 8);
        assert_eq!(m3.hears[0], MixId::new("member1"));
        assert!(!m3.hears.contains(&MixId::new("member3")));
        // member1 hears the other members as peers, member2 first.
        assert_eq!(
            v.mix(&MixId::new("member1")).unwrap().hears[0],
            MixId::new("member2")
        );
        // A mix that hears both: its heard mixes, then its peers.
        let mut topo = test_topology();
        let m2 = topo
            .mixes
            .iter_mut()
            .find(|m| m.id == MixId::new("member2"))
            .unwrap();
        m2.mixes = vec![MixId::new("member4")];
        m2.peers = vec![MixId::new("member9"), MixId::new("member1")];
        let (v, problems) = SiteView::build(&test_config(), &topo);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            v.mix(&MixId::new("member2")).unwrap().hears,
            [
                MixId::new("member4"),
                MixId::new("member9"),
                MixId::new("member1")
            ]
        );
    }

    fn roster_view(active: &str) -> SiteView {
        let roster = crate::roster_state::tests::test_roster(active);
        let (view, problems) =
            SiteView::build_with(&test_config(), Some(&roster), &test_topology());
        assert_eq!(problems, Vec::<String>::new());
        view
    }

    /// #58: in roster mode the active lineup's people are the members, each
    /// on their set; a set nobody wears is no page.
    #[test]
    fn roster_mode_maps_people_to_sets_of_the_active_lineup() {
        let v = roster_view("youth");
        let members: Vec<(&str, &str, &str)> = v
            .members
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str(), m.mix.as_str()))
            .collect();
        assert_eq!(
            members,
            [
                ("member10", "Member 10", "member1"),
                ("member3", "Member 3", "member3"),
                ("member11", "Member 11", "member4"),
                ("engineer", "Engineer", "engineer"),
            ]
        );
        let p = v.page("member10").unwrap();
        assert_eq!(
            (p.name.as_str(), p.mix.0.as_str(), p.member.as_deref()),
            ("Member 10", "member1", Some("member10"))
        );
        assert!(
            v.page("member2").is_none(),
            "member2 plays in no set of youth"
        );
        assert!(v.page("member1").is_none(), "member1's set is member10's");
        assert!(v.page("member4").is_none(), "member4's set is member11's");
        assert_eq!(v.page("translator").unwrap().member, None);
        let pages: Vec<String> = v.mix_pages().into_iter().map(|p| p.id).collect();
        assert_eq!(pages, ["translator"], "vacant sets are no pages");
        let vacant: Vec<&str> = v.vacant.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(
            vacant,
            [
                "member2", "member5", "member6", "member7", "member8", "member9"
            ]
        );
        let set = v.mix(&MixId::new("member1")).unwrap();
        assert_eq!(
            (set.name.as_str(), set.member.as_deref()),
            ("Member 10", Some("member10"))
        );
        assert_eq!(v.mix_name(&MixId::new("member2")), "Member2");
        assert_eq!(
            v.member_of_mix(&MixId::new("member4"))
                .map(|m| m.id.as_str()),
            Some("member11")
        );
        assert_eq!(v.leaders, ["member3"]);
        assert_eq!(v.people.len(), 12);
        assert!(v.people[0].playing && !v.people[3].playing);
        // Main: everyone on their own set, both leaders, no vacant set.
        let m = roster_view("main");
        assert_eq!(m.members.len(), 10);
        assert_eq!(m.member("member2").unwrap().name, "Member 2");
        assert_eq!(m.page("member2").unwrap().mix, MixId::new("member2"));
        assert_eq!(m.leaders, ["member1", "member3"]);
        assert!(m.vacant.is_empty());
    }

    /// #58, the owner's look at the live system: a person's set no lineup
    /// lists (member9 left; the site's legacy `[[members]]` still give them
    /// set member9) is vacant like a set the other lineup wears, whichever
    /// lineup plays: no page. The engineer's and the translator's mixes
    /// never are. Legacy mode keeps member9 on their set, as before.
    #[test]
    fn a_departed_persons_set_is_vacant_and_no_page() {
        let view = |active: &str| {
            let roster = crate::roster_state::tests::departed_roster(active);
            let (v, problems) =
                SiteView::build_with(&test_config(), Some(&roster), &test_topology());
            assert_eq!(problems, Vec::<String>::new());
            v
        };
        let vacant =
            |v: &SiteView| -> Vec<String> { v.vacant.iter().map(|m| m.0.clone()).collect() };
        let main = view("main");
        assert_eq!(vacant(&main), ["member9"]);
        let youth = view("youth");
        assert_eq!(
            vacant(&youth),
            [
                "member2", "member5", "member6", "member7", "member8", "member9"
            ]
        );
        for v in [&main, &youth] {
            assert!(v.page("member9").is_none(), "nobody wears set member9");
            let pages: Vec<String> = v.mix_pages().into_iter().map(|p| p.id).collect();
            assert_eq!(pages, ["translator"], "the translator's is no person's set");
            assert_eq!(v.page("engineer").unwrap().mix, MixId::new("engineer"));
            assert_eq!(v.mix_name(&MixId::new("member9")), "Member9");
        }
        assert_eq!(
            main.page("member8").unwrap().member.as_deref(),
            Some("member8")
        );
        // Legacy: member9 is a member on their own set.
        let legacy = test_view();
        assert!(legacy.vacant.is_empty());
        let p = legacy.page("member9").unwrap();
        assert_eq!(
            (p.mix.0.as_str(), p.member.as_deref()),
            ("member9", Some("member9"))
        );
    }

    /// #58: an input the lineup lists takes its name and owner; any other
    /// keeps the site's label, and only the engineer keeps an owner there.
    #[test]
    fn input_names_follow_the_lineup() {
        let v = roster_view("youth");
        let input = |id: &str| {
            let i = v.input(id).unwrap();
            (i.name.clone(), i.owner.clone(), i.category.clone())
        };
        let owned = |name: &str, owner: Option<&str>, category: &str| {
            (
                name.to_string(),
                owner.map(String::from),
                category.to_string(),
            )
        };
        assert_eq!(
            input("mic1"),
            owned("MEMBER10 mic", Some("member10"), "mics")
        );
        assert_eq!(
            input("mic4"),
            owned("MEMBER11 mic", Some("member11"), "mics")
        );
        assert_eq!(input("mic2"), owned("MEMBER2 mic", None, "mics"));
        assert_eq!(
            input("eng_mic"),
            owned("ENGINEER mic", Some("engineer"), "tech")
        );
        assert_eq!(input("drums"), owned("DRUMS", None, "stems"));
        assert_eq!(v.own_input("member10"), Some(&InputId::new("mic1")));
        assert_eq!(v.own_input("member2"), None);
        assert_eq!(v.own_input("engineer"), Some(&InputId::new("eng_mic")));
        // Main names and owns them as the site does.
        let m = roster_view("main");
        let gtr = m.input("mic5").unwrap();
        assert_eq!(
            (gtr.name.as_str(), gtr.owner.as_deref()),
            ("MEMBER4 gtr", Some("member4"))
        );
        // A lineup's input the engine lacks is reported and left out.
        let mut roster = crate::roster_state::tests::test_roster("youth");
        let youth = std::sync::Arc::make_mut(&mut roster.roster)
            .lineups
            .iter_mut()
            .find(|l| l.id == "youth")
            .unwrap();
        youth.inputs.insert(
            "ghost".into(),
            iem_core::roster::LineupInput {
                name: "GHOST".into(),
                owner: None,
            },
        );
        let (v, problems) = SiteView::build_with(&test_config(), Some(&roster), &test_topology());
        assert_eq!(
            problems,
            ["lineup 'youth': input 'ghost' is not an engine input"]
        );
        assert!(v.input("ghost").is_none());
        assert_eq!(v.inputs.len(), 24);
    }

    /// #58: without a roster the view is the one the site always gave.
    #[test]
    fn legacy_mode_is_unchanged() {
        let (config, topo) = (test_config(), test_topology());
        let (with, problems) = SiteView::build_with(&config, None, &topo);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(with.members, config.members);
        assert_eq!(
            with.leaders,
            ["member1", "member3"],
            "members whose mix hears another; the engineer is no leader"
        );
        assert_eq!(with.people.len(), 10);
        assert!(with.people.iter().all(|p| p.playing));
        assert_eq!(
            with.people[2],
            PersonView {
                id: "member3".into(),
                name: "Member3".into(),
                playing: true,
            }
        );
        assert!(with.vacant.is_empty());
        let mic2 = with.input("mic2").unwrap();
        assert_eq!(
            (mic2.name.as_str(), mic2.owner.as_deref()),
            ("MEMBER2 mic", Some("member2"))
        );
        // A member whose mix hears only whole mixes leads too.
        let mut topo = test_topology();
        let m2 = topo
            .mixes
            .iter_mut()
            .find(|m| m.id == MixId::new("member2"))
            .unwrap();
        m2.mixes = vec![MixId::new("member4")];
        let (v, _) = SiteView::build(&config, &topo);
        assert_eq!(v.leaders, ["member1", "member2", "member3"]);
    }

    #[test]
    fn pages_are_members_or_member_less_mixes() {
        let v = test_view();
        let p = v.page("member3").unwrap();
        assert_eq!(
            (p.name.as_str(), p.mix.0.as_str(), p.member.as_deref()),
            ("Member3", "member3", Some("member3"))
        );
        let t = v.page("translator").unwrap();
        assert_eq!((t.name.as_str(), t.member), ("Translator", None));
        assert!(v.page("member10").is_none());
        assert!(v.page("mic1").is_none());
        let pages: Vec<String> = v.mix_pages().into_iter().map(|p| p.id).collect();
        assert_eq!(pages, ["translator"]);
    }

    #[test]
    fn mismatches_are_reported_and_left_out() {
        let mut config = test_config();
        config.members.push(SiteMember {
            id: "member10".into(),
            name: "Member10".into(),
            mix: "member10".into(),
        });
        config.inputs.retain(|i| i.id != "keys");
        config.inputs.push(iem_core::config::SiteInputMeta {
            id: "ghost".into(),
            name: "GHOST".into(),
            category: None,
            owner: None,
        });
        let (v, problems) = SiteView::build(&config, &test_topology());
        assert_eq!(
            problems,
            [
                "member 'member10': the engine has no mix 'member10'",
                "input 'keys' has no [[inputs]] label",
                "[[inputs]] 'ghost' is not an engine input",
            ]
        );
        assert!(v.member("member10").is_none());
        let keys = v.input("keys").unwrap();
        assert_eq!(
            (keys.name.as_str(), keys.category.as_str()),
            ("KEYS", "mics")
        );
        assert!(v.input("ghost").is_none());
        assert_eq!(title(""), "");
    }
}
