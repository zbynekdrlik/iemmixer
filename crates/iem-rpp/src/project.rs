//! Project model → RPP text for the golden renders.

use crate::fx::{FxSlot, fx_chain, guid};
use crate::rpp::{Chunk, RppError, num, q};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    PostFader,
    PreFx,
    PreFader,
}

impl SendMode {
    pub const fn code(self) -> u8 {
        match self {
            Self::PostFader => 0,
            Self::PreFx => 1,
            Self::PreFader => 3,
        }
    }
}

/// A receive on the destination track (REAPER's `AUXRECV`).
#[derive(Debug, Clone, PartialEq)]
pub struct Send {
    pub src: usize,
    pub mode: SendMode,
    pub vol: f64,
    pub pan: f64,
    pub mute: bool,
    /// Destination channel field 1024: mix to mono into channel 1.
    pub dst_mono: bool,
}

impl Send {
    pub const fn new(src: usize, mode: SendMode) -> Self {
        Self {
            src,
            mode,
            vol: 1.0,
            pan: 0.0,
            mute: false,
            dst_mono: false,
        }
    }
}

/// A media item; position and length in samples at the project rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub stimulus: String,
    pub position: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: String,
    pub vol: f64,
    pub pan: f64,
    pub mute: bool,
    pub fx_enabled: bool,
    pub fx: Vec<FxSlot>,
    pub item: Option<Item>,
    pub receives: Vec<Send>,
    /// Selected, so it renders as a stem.
    pub render: bool,
}

impl Track {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            vol: 1.0,
            pan: 0.0,
            mute: false,
            fx_enabled: true,
            fx: Vec::new(),
            item: None,
            receives: Vec::new(),
            render: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Float64,
    Float32,
}

impl RenderFormat {
    /// `RENDER_CFG` payload "evaw" + bit depth + 0 + 1 (UNVERIFIED until Task 13).
    pub const fn cfg(self) -> &'static str {
        match self {
            Self::Float64 => "ZXZhd0AAAQ==",
            Self::Float32 => "ZXZhdyAAAQ==",
        }
    }

    pub const fn bits(self) -> u16 {
        match self {
            Self::Float64 => 64,
            Self::Float32 => 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: String,
    pub rate: u32,
    pub format: RenderFormat,
    pub tracks: Vec<Track>,
}

fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Project {
    pub fn validate(&self) -> Result<(), RppError> {
        let bad = |m: String| Err(RppError::Invalid(m));
        if !safe_name(&self.id) {
            return bad(format!("project id {:?}", self.id));
        }
        if ![44_100, 48_000, 96_000].contains(&self.rate) {
            return bad(format!("rate {}", self.rate));
        }
        let mut names = std::collections::BTreeSet::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if !safe_name(&t.name) || !names.insert(t.name.as_str()) {
                return bad(format!("track name {:?} (unsafe or duplicate)", t.name));
            }
            if !(t.vol.is_finite() && t.vol >= 0.0 && (-1.0..=1.0).contains(&t.pan)) {
                return bad(format!("track {} vol/pan", t.name));
            }
            if let Some(item) = &t.item {
                if !safe_name(&item.stimulus) || item.length == 0 {
                    return bad(format!("track {} item", t.name));
                }
            }
            for s in &t.receives {
                if s.src >= i
                    || !(s.vol.is_finite() && s.vol >= 0.0 && (-1.0..=1.0).contains(&s.pan))
                {
                    return bad(format!(
                        "track {} receive from {} (sources precede, gains finite)",
                        t.name, s.src
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn to_rpp(&self) -> Result<String, RppError> {
        self.validate()?;
        let mut p = Chunk::new(r#"REAPER_PROJECT 0.1 "7.65/win64" 0 0"#);
        p.line("PANLAW 1").line("PANMODE 3");
        p.line(format!("SAMPLERATE {} 1 0", self.rate));
        p.line("TEMPO 120 4 4 0");
        p.line(format!("RENDER_FILE \"@@OUT@@\\{}\"", self.id));
        p.line("RENDER_PATTERN $track")
            .line("RENDER_FMT 0 2 0")
            .line("RENDER_1X 0");
        p.line("RENDER_RANGE 1 0 0 18 1000")
            .line("RENDER_RESAMPLE 3 0 1")
            .line("RENDER_ADDTOPROJ 0");
        p.line("RENDER_STEMS 2")
            .line("RENDER_DITHER 0")
            .line("RENDER_TRIM 0.000001 0.000001 0 0");
        let mut cfg = Chunk::new("RENDER_CFG");
        cfg.line(self.format.cfg());
        p.child(cfg);
        p.line("MASTER_NCH 2 2")
            .line("MASTER_VOLUME 1 0 -1 -1 1")
            .line("MASTER_PANMODE 3")
            .line("MASTERMUTESOLO 0");
        for t in &self.tracks {
            p.child(self.track_chunk(t)?);
        }
        Ok(p.render())
    }

    fn track_chunk(&self, t: &Track) -> Result<Chunk, RppError> {
        let id = guid(&format!("{}/{}", self.id, t.name));
        let mut c = Chunk::new(format!("TRACK {id}"));
        c.line(format!("NAME {}", q(&t.name)?));
        c.line("PANLAWFLAGS 3");
        c.line(format!("VOLPAN {} {} -1 -1 1", num(t.vol)?, num(t.pan)?));
        c.line(format!("MUTESOLO {} 0 0", u8::from(t.mute)));
        c.line("IPHASE 0");
        c.line(format!("SEL {}", u8::from(t.render)));
        c.line("REC 0 0 0 0 0 0 0 0");
        c.line("NCHAN 2");
        c.line(format!("FX {}", u8::from(t.fx_enabled)));
        c.line(format!("TRACKID {id}"));
        c.line("MAINSEND 0 0");
        for s in &t.receives {
            c.line(format!(
                "AUXRECV {} {} {} {} {} 0 0 0 {} -1:U 0 -1 ''",
                s.src,
                s.mode.code(),
                num(s.vol)?,
                num(s.pan)?,
                u8::from(s.mute),
                if s.dst_mono { 1024 } else { 0 }
            ));
        }
        if !t.fx.is_empty() {
            c.child(fx_chain(&t.fx, &format!("{}/{}", self.id, t.name))?);
        }
        if let Some(item) = &t.item {
            c.child(self.item_chunk(item)?);
        }
        Ok(c)
    }

    #[allow(clippy::cast_precision_loss)]
    fn item_chunk(&self, item: &Item) -> Result<Chunk, RppError> {
        let fs = f64::from(self.rate);
        let mut c = Chunk::new("ITEM");
        c.line(format!("POSITION {}", num(item.position as f64 / fs)?));
        c.line(format!("LENGTH {}", num(item.length as f64 / fs)?));
        c.line("LOOP 0")
            .line("FADEIN 1 0 0 1 0 0 0")
            .line("FADEOUT 1 0 0 1 0 0 0")
            .line("MUTE 0 0");
        c.line(format!("NAME {}", q(&item.stimulus)?));
        c.line("VOLPAN 1 0 1 -1")
            .line("SOFFS 0")
            .line("PLAYRATE 1 1 0 -1 0 0.0025")
            .line("CHANMODE 0");
        let mut src = Chunk::new("SOURCE WAVE");
        src.line(format!("FILE \"@@JOB@@\\stimuli\\{}\"", item.stimulus));
        c.child(src);
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_track() -> Project {
        let mut src = Track::new("src");
        src.render = false;
        src.item = Some(Item {
            stimulus: "imp-dm-96000.wav".into(),
            position: 0,
            length: 48_000,
        });
        let mut bus = Track::new("bus");
        let mut s = Send::new(0, SendMode::PreFader);
        s.pan = 0.5;
        s.dst_mono = true;
        bus.receives.push(s);
        Project {
            id: "p".into(),
            rate: 96_000,
            format: RenderFormat::Float64,
            tracks: vec![src, bus],
        }
    }

    #[test]
    fn writes_render_settings_tokens_and_track_fields() {
        let text = two_track().to_rpp().unwrap();
        for needle in [
            "<REAPER_PROJECT 0.1 \"7.65/win64\" 0 0\n",
            "\n  SAMPLERATE 96000 1 0\n",
            "\n  RENDER_FILE \"@@OUT@@\\p\"\n",
            "\n  RENDER_STEMS 2\n",
            "\n  <RENDER_CFG\n    ZXZhd0AAAQ==\n  >\n",
            "\n    SEL 0\n",
            "\n    AUXRECV 0 3 1 0.5 0 0 0 0 1024 -1:U 0 -1 ''\n",
            "\n        FILE \"@@JOB@@\\stimuli\\imp-dm-96000.wav\"\n",
            "\n      POSITION 0\n      LENGTH 0.5\n",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
    }

    #[test]
    fn refuses_cycles_duplicates_and_unsafe_names() {
        let mut p = two_track();
        p.tracks[1].receives[0].src = 1;
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[1].name = "src".into();
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[0].name = "a b".into();
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[1].receives[0].pan = 1.5;
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.rate = 22_050;
        assert!(p.to_rpp().is_err());
    }

    #[test]
    fn float32_format_changes_only_the_render_cfg() {
        let mut p = two_track();
        p.format = RenderFormat::Float32;
        assert!(p.to_rpp().unwrap().contains("\n    ZXZhdyAAAQ==\n"));
        assert_eq!(RenderFormat::Float32.bits(), 32);
    }
}
