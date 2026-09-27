//! The site as the UI sees it (S5 design note §3, §6): the configured members
//! and input labels joined with the engine's topology. Built again whenever
//! the engine announces a topology; ids the topology does not have are
//! reported and left out, never guessed.

use iem_core::config::{Config, SiteMember};
use iem_engine_proto::{GroupId, InputId, MixId, TopologyInfo};

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
    /// The mixes it hears (its Mixes tab), in topology order.
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
    /// The view of `config` over `topo`, and every mismatch between them.
    pub fn build(config: &Config, topo: &TopologyInfo) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let mix_ids: Vec<&MixId> = topo.mixes.iter().map(|m| &m.id).collect();
        let members: Vec<SiteMember> = config
            .members
            .iter()
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
            .cloned()
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
                InputView {
                    id: i.id.clone(),
                    name: meta.map_or_else(|| i.id.0.to_uppercase(), |m| m.name.clone()),
                    category,
                    owner: meta.and_then(|m| m.owner.clone()),
                    group: i.group.clone(),
                }
            })
            .collect();
        for m in &config.inputs {
            if !topo.inputs.iter().any(|i| i.id.0 == m.id) {
                problems.push(format!("[[inputs]] '{}' is not an engine input", m.id));
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
                    hears: m.mixes.clone(),
                    mono: m.channels == 1,
                }
            })
            .collect();
        let view = Self {
            members,
            inputs,
            mixes,
            group: topo.groups.first().map(|g| g.id.clone()),
            engineer_mix: Some(topo.engineer.clone()),
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

    /// The page `id`: a member, or a mix without a member.
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
            .find(|m| m.id.0 == id && m.member.is_none())
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
