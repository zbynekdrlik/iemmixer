//! Meters for the UI (F9; S5 design note §5): the engine publishes peaks at
//! 30 Hz; the server max-merges them and sends every page a map by id every
//! 100 ms — inputs (post-mute), mixes (post volume and mute) and the page's
//! own stems strip under the group id.

use std::collections::HashMap;

use iem_engine_proto::{GroupId, Meters, TopologyInfo};

use crate::site_view::Page;

/// How often pages get meters.
pub const METER_PERIOD_MS: u64 = 100;

/// Peaks merged since the last send, and the latest limiter counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Merged {
    pub inputs: Vec<[f32; 2]>,
    pub mixes: Vec<[f32; 2]>,
    /// Mix-major: mix `m`, group `g` at `m · groups + g`.
    pub groups: Vec<[f32; 2]>,
    /// Limiter active seconds per mix (X14), in topology order.
    pub active_s: Vec<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MeterMerge {
    cur: Merged,
    fresh: bool,
    active_s: Vec<f64>,
}

fn merge(into: &mut Vec<[f32; 2]>, from: &[[f32; 2]]) {
    if into.len() < from.len() {
        into.resize(from.len(), [0.0, 0.0]);
    }
    for (a, b) in into.iter_mut().zip(from) {
        a[0] = a[0].max(b[0]);
        a[1] = a[1].max(b[1]);
    }
}

impl MeterMerge {
    pub fn push(&mut self, m: &Meters) {
        merge(&mut self.cur.inputs, &m.inputs);
        merge(&mut self.cur.mixes, &m.mixes);
        merge(&mut self.cur.groups, &m.groups);
        self.active_s.clone_from(&m.limiter_active_s);
        self.fresh = true;
    }

    /// The peaks since the last `take` (`None` when no frame came).
    pub fn take(&mut self) -> Option<Merged> {
        if !self.fresh {
            return None;
        }
        self.fresh = false;
        let mut out = std::mem::take(&mut self.cur);
        out.active_s = self.active_s.clone();
        Some(out)
    }

    /// The latest limiter active seconds per mix.
    pub fn active_s(&self) -> &[f64] {
        &self.active_s
    }
}

/// The meter map one page shows.
pub fn page_meters(
    m: &Merged,
    topo: &TopologyInfo,
    page: &Page,
    group: Option<&GroupId>,
) -> HashMap<String, [f32; 2]> {
    let mut out = HashMap::new();
    for (info, v) in topo.inputs.iter().zip(&m.inputs) {
        out.insert(info.id.0.clone(), *v);
    }
    for (info, v) in topo.mixes.iter().zip(&m.mixes) {
        out.insert(info.id.0.clone(), *v);
    }
    let groups = topo.groups.len();
    let mix = topo.mixes.iter().position(|x| x.id == page.mix);
    let g = group.and_then(|g| topo.groups.iter().position(|x| &x.id == g));
    if let (Some(mix), Some(g), Some(group)) = (mix, g, group)
        && let Some(v) = m.groups.get(mix * groups + g)
    {
        out.insert(group.0.clone(), *v);
    }
    out
}

/// The limiter active seconds of `mix` (0 when unknown).
pub fn active_seconds(active_s: &[f64], topo: &TopologyInfo, mix: &iem_engine_proto::MixId) -> f64 {
    topo.mixes
        .iter()
        .position(|x| &x.id == mix)
        .and_then(|i| active_s.get(i).copied())
        .unwrap_or(0.0)
}

/// The loudest input peak of a frame (band activity, §4.2).
pub fn max_input_peak(m: &Meters) -> f32 {
    m.inputs
        .iter()
        .flat_map(|p| p.iter().copied())
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site_view::tests::{test_topology, test_view};
    use iem_engine_proto::MixId;

    fn frame(inputs: Vec<[f32; 2]>, groups: Vec<[f32; 2]>, active: Vec<f64>) -> Meters {
        Meters {
            seq: 1,
            mixes: vec![[0.1, 0.2]; 11],
            inputs,
            groups,
            gr_db: vec![0.0; 11],
            limiter_active_s: active,
            trips: 0,
        }
    }

    #[test]
    fn frames_merge_by_maximum_until_taken() {
        let mut mm = MeterMerge::default();
        assert_eq!(mm.take(), None);
        mm.push(&frame(vec![[0.5, 0.1], [0.0, 0.0]], vec![], vec![1.0]));
        mm.push(&frame(vec![[0.2, 0.3]], vec![[0.4, 0.4]], vec![2.5]));
        let m = mm.take().unwrap();
        assert_eq!(m.inputs, vec![[0.5, 0.3], [0.0, 0.0]]);
        assert_eq!(m.groups, vec![[0.4, 0.4]]);
        assert_eq!(m.mixes.len(), 11);
        assert_eq!(m.active_s, vec![2.5]);
        assert_eq!(mm.active_s(), &[2.5]);
        assert_eq!(mm.take(), None, "nothing new");
        mm.push(&frame(vec![[0.1, 0.1]], vec![], vec![3.0]));
        assert_eq!(mm.take().unwrap().inputs, vec![[0.1, 0.1]], "peaks restart");
    }

    #[test]
    fn a_page_sees_inputs_mixes_and_its_own_stems_strip() {
        let topo = test_topology();
        let v = test_view();
        let mut groups = vec![[0.0, 0.0]; 11];
        let m1 = topo.mixes.iter().position(|m| m.id.0 == "member1").unwrap();
        groups[m1] = [0.7, 0.6];
        let merged = Merged {
            inputs: (0..24).map(|i| [i as f32 / 100.0, 0.0]).collect(),
            mixes: vec![[0.3, 0.3]; 11],
            groups,
            active_s: vec![],
        };
        let p = v.page("member1").unwrap();
        let map = page_meters(&merged, &topo, &p, v.group.as_ref());
        assert_eq!(map.len(), 24 + 11 + 1);
        assert_eq!(map["mic2"], [0.01, 0.0]);
        assert_eq!(map["member1"], [0.3, 0.3]);
        assert_eq!(map["stems"], [0.7, 0.6]);
        let p2 = v.page("member2").unwrap();
        assert_eq!(
            page_meters(&merged, &topo, &p2, v.group.as_ref())["stems"],
            [0.0, 0.0]
        );
        assert!(!page_meters(&merged, &topo, &p2, None).contains_key("stems"));
    }

    #[test]
    fn active_seconds_and_input_peaks() {
        let topo = test_topology();
        let e = topo
            .mixes
            .iter()
            .position(|m| m.id.0 == "engineer")
            .unwrap();
        let mut active = vec![0.0; 11];
        active[e] = 42.0;
        assert_eq!(
            active_seconds(&active, &topo, &MixId::new("engineer")),
            42.0
        );
        assert_eq!(active_seconds(&active, &topo, &MixId::new("nope")), 0.0);
        assert_eq!(active_seconds(&[], &topo, &MixId::new("engineer")), 0.0);
        assert_eq!(
            max_input_peak(&frame(vec![[0.1, 0.4], [0.3, 0.2]], vec![], vec![])),
            0.4
        );
        assert_eq!(max_input_peak(&Meters::default()), 0.0);
    }
}
