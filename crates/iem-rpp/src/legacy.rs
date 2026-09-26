//! The predecessor's saved project as the importer reads it (S4 design note
//! §3.2): per track the lines that carry mix values (their line indices, so
//! the exporter can patch them), receives, hardware outputs and the FX chain;
//! plus the master's lines.

use crate::read::{self, Block, Doc, Item};
use crate::rpp::RppError;

fn invalid(line: usize, msg: impl std::fmt::Display) -> RppError {
    RppError::Invalid(format!("line {}: {msg}", line + 1))
}

fn field(t: &[String], k: usize, line: usize) -> Result<&str, RppError> {
    t.get(k)
        .map(String::as_str)
        .ok_or_else(|| invalid(line, format!("field {k} missing")))
}

/// A finite number.
pub fn float(t: &[String], k: usize, line: usize) -> Result<f64, RppError> {
    let s = field(t, k, line)?;
    s.parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
        .ok_or_else(|| invalid(line, format!("field {k} {s:?} is not a finite number")))
}

pub fn int(t: &[String], k: usize, line: usize) -> Result<i64, RppError> {
    let s = field(t, k, line)?;
    s.parse::<i64>()
        .map_err(|_| invalid(line, format!("field {k} {s:?} is not an integer")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    /// `JS utility/volume_pan` (the input trim).
    Trim,
    /// `VST: ReaEQ (Cockos)`.
    ReaEq,
    /// `JS loser/MGA_JSLimiterST` (the output limiter).
    Limiter,
    /// The predecessor's talkback injector (`OIEM Receive`).
    Talkback,
    /// The predecessor's listen tap (`VBAN IEM`).
    Listen,
    /// Anything else, including automation envelopes in the chain.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    pub kind: PluginKind,
    /// The display name or path from the chunk head.
    pub name: String,
    pub bypassed: bool,
    /// The `BYPASS` line before the plug-in.
    pub bypass_line: Option<usize>,
    pub block: Block,
}

/// An `AUXRECV` line: a send into the track that holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Receive {
    pub line: usize,
    /// 0-based source track.
    pub src: usize,
    pub mode: i64,
    pub vol: f64,
    pub pan: f64,
    pub mute: bool,
    /// Mono sum, phase, source and destination channel fields are all 0.
    pub plain: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: String,
    pub volpan: Option<usize>,
    pub vol: f64,
    pub pan: f64,
    pub mutesolo: Option<usize>,
    pub muted: bool,
    pub soloed: bool,
    /// `REC` armed flag and input field.
    pub rec: Option<(bool, i64)>,
    pub fx: Option<usize>,
    pub fx_on: bool,
    pub hwout: Vec<i64>,
    pub receives: Vec<Receive>,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Master {
    pub volume: usize,
    pub vol: f64,
    pub pan: f64,
    pub mutesolo: Option<usize>,
    pub mute_flags: i64,
    pub hwout: Vec<i64>,
    /// Plug-ins in the master chain that are not bypassed.
    pub active_fx: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyProject {
    pub doc: Doc,
    pub tracks: Vec<Track>,
    pub master: Master,
}

const PLUGIN_CHUNKS: [&str; 7] = ["VST", "JS", "AU", "DX", "CLAP", "LV2", "VIDEO_EFFECT"];

fn classify(head: &[String]) -> (PluginKind, String) {
    let kind_token = head.first().map_or("", String::as_str);
    let name = head
        .get(1)
        .cloned()
        .unwrap_or_else(|| kind_token.to_owned());
    if !PLUGIN_CHUNKS.contains(&kind_token) {
        return (PluginKind::Other, kind_token.to_owned());
    }
    let kind = match (kind_token, name.as_str()) {
        ("JS", "utility/volume_pan") => PluginKind::Trim,
        ("JS", "loser/MGA_JSLimiterST") => PluginKind::Limiter,
        ("VST", "VST: ReaEQ (Cockos)") => PluginKind::ReaEq,
        (_, n) if n.contains("OIEM Receive") => PluginKind::Talkback,
        (_, n) if n.contains("VBAN IEM") => PluginKind::Listen,
        _ => PluginKind::Other,
    };
    (kind, name)
}

fn parse_chain(doc: &Doc, chain: &Block) -> Result<Vec<Plugin>, RppError> {
    let mut plugins = Vec::new();
    let mut pending: Option<(usize, bool)> = None;
    for item in chain.items() {
        match item {
            Item::Line(i) => {
                let t = doc.tokens(i);
                if t.first().map(String::as_str) == Some("BYPASS") {
                    pending = Some((i, int(&t, 1, i)? != 0));
                }
            }
            Item::Chunk(block) => {
                let (kind, name) = classify(&doc.head_tokens(block));
                let (bypass_line, bypassed) = match pending.take() {
                    Some((line, b)) => (Some(line), b),
                    None => (None, false),
                };
                plugins.push(Plugin {
                    kind,
                    name,
                    bypassed,
                    bypass_line,
                    block: block.clone(),
                });
            }
        }
    }
    Ok(plugins)
}

fn parse_track(doc: &Doc, block: &Block) -> Result<Track, RppError> {
    let mut t = Track {
        name: String::new(),
        volpan: None,
        vol: 1.0,
        pan: 0.0,
        mutesolo: None,
        muted: false,
        soloed: false,
        rec: None,
        fx: None,
        fx_on: true,
        hwout: Vec::new(),
        receives: Vec::new(),
        plugins: Vec::new(),
    };
    for i in block.direct() {
        let tok = doc.tokens(i);
        match tok.first().map(String::as_str) {
            Some("NAME") => t.name = tok.get(1).cloned().unwrap_or_default(),
            Some("VOLPAN") => {
                t.volpan = Some(i);
                t.vol = float(&tok, 1, i)?;
                t.pan = float(&tok, 2, i)?;
            }
            Some("MUTESOLO") => {
                t.mutesolo = Some(i);
                t.muted = int(&tok, 1, i)? != 0;
                t.soloed = int(&tok, 2, i)? != 0;
            }
            Some("REC") => t.rec = Some((int(&tok, 1, i)? != 0, int(&tok, 2, i)?)),
            Some("FX") => {
                t.fx = Some(i);
                t.fx_on = int(&tok, 1, i)? != 0;
            }
            Some("HWOUT") => t.hwout.push(int(&tok, 1, i)?),
            Some("AUXRECV") => {
                let src = usize::try_from(int(&tok, 1, i)?)
                    .map_err(|_| invalid(i, "negative source track"))?;
                let mut plain = true;
                for k in 6..=9 {
                    plain &= int(&tok, k, i)? == 0;
                }
                t.receives.push(Receive {
                    line: i,
                    src,
                    mode: int(&tok, 2, i)?,
                    vol: float(&tok, 3, i)?,
                    pan: float(&tok, 4, i)?,
                    mute: int(&tok, 5, i)? != 0,
                    plain,
                });
            }
            _ => {}
        }
    }
    for child in &block.children {
        if doc.chunk_name(child) == "FXCHAIN" {
            t.plugins.extend(parse_chain(doc, child)?);
        }
    }
    Ok(t)
}

fn parse_master(doc: &Doc, root: &Block) -> Result<Master, RppError> {
    let mut volume = None;
    let mut m = Master {
        volume: 0,
        vol: 1.0,
        pan: 0.0,
        mutesolo: None,
        mute_flags: 0,
        hwout: Vec::new(),
        active_fx: 0,
    };
    for i in root.direct() {
        let tok = doc.tokens(i);
        match tok.first().map(String::as_str) {
            Some("MASTER_VOLUME") => {
                volume = Some(i);
                m.vol = float(&tok, 1, i)?;
                m.pan = float(&tok, 2, i)?;
            }
            Some("MASTERMUTESOLO") => {
                m.mutesolo = Some(i);
                m.mute_flags = int(&tok, 1, i)?;
            }
            Some("MASTERHWOUT") => m.hwout.push(int(&tok, 1, i)?),
            _ => {}
        }
    }
    for child in &root.children {
        if doc.chunk_name(child) == "MASTERFXLIST" {
            m.active_fx += parse_chain(doc, child)?
                .iter()
                .filter(|p| !p.bypassed)
                .count();
        }
    }
    m.volume = volume.ok_or_else(|| RppError::Invalid("no MASTER_VOLUME line".into()))?;
    Ok(m)
}

impl LegacyProject {
    pub fn parse(text: &str) -> Result<Self, RppError> {
        let (doc, root) = read::parse(text)?;
        if doc.chunk_name(&root) != "REAPER_PROJECT" {
            return Err(RppError::Invalid("not a REAPER project".into()));
        }
        let mut tracks = Vec::new();
        for child in &root.children {
            if doc.chunk_name(child) == "TRACK" {
                tracks.push(parse_track(&doc, child)?);
            }
        }
        let master = parse_master(&doc, &root)?;
        Ok(Self {
            doc,
            tracks,
            master,
        })
    }

    /// The body lines of a plug-in chunk (its own lines, in order).
    pub fn body(&self, plugin: &Plugin) -> Vec<&str> {
        plugin
            .block
            .direct()
            .into_iter()
            .map(|i| self.doc.content(i))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"<REAPER_PROJECT 0.1 "7.65/win64" 0 0
  MASTERMUTESOLO 1
  MASTERHWOUT 0 0 1 0 0 0 0 -1
  MASTER_VOLUME 0.5 -0.25 -1 -1 1
  <MASTERFXLIST
    BYPASS 1 0 0
    <JS some/thing ""
      0 -
    >
  >
  <TRACK {A}
    NAME "IN 1"
    VOLPAN 1 0 -1 -1 1
    MUTESOLO 0 0 0
    REC 1 4 1 2 0 0 0 0
    FX 0
    <FXCHAIN
      SHOW 0
      BYPASS 0 0 0
      <JS utility/volume_pan "TRIM IN"
        3 0 0 -
      >
      FXID {B}
      BYPASS 1 0 0
      <JS synthesis/tonegenerator ""
        -12 -
      >
      <VST "VST3: OIEM Receive (Test)" t.vst3 0 "" 1{00} ""
        AAAA
      >
    >
  >
  <TRACK {C}
    NAME OUT
    VOLPAN 0.25 0.5 -1 -1 1
    MUTESOLO 1 1 1
    REC 0 -1 0 2 0 0 0 0
    HWOUT 2 0 1 0 0 0 0 -1:U -1
    AUXRECV 0 3 0.5 -1 1 0 0 0 0 -1:U 0 -1 ''
    AUXRECV 0 0 1 0 0 1 0 0 0 -1:U 0 -1 ''
    <FXCHAIN
      <VST "VST: ReaEQ (Cockos)" reaeq.dll 0 EQ 1{00} ""
        AAAA
      >
      BYPASS 1 0 0
      <JS loser/MGA_JSLimiterST LIMITER
        -6 50 75 -6 0 -
      >
      <VST "VST3: VBAN IEM (Test)" v.vst3 0 "" 1{00} ""
        AAAA
      >
      <PARMENV 1 0 1 0
      >
    >
  >
>
"#;

    #[test]
    fn reads_tracks_receives_plugins_and_master() {
        let p = LegacyProject::parse(TEXT).unwrap();
        assert_eq!(p.tracks.len(), 2);
        let a = &p.tracks[0];
        assert_eq!(a.name, "IN 1");
        assert_eq!(a.rec, Some((true, 4)));
        assert!(!a.fx_on);
        assert_eq!(p.doc.content(a.fx.unwrap()), "FX 0");
        assert_eq!(p.doc.content(a.volpan.unwrap()), "VOLPAN 1 0 -1 -1 1");
        assert_eq!(
            a.plugins.iter().map(|x| x.kind).collect::<Vec<_>>(),
            vec![PluginKind::Trim, PluginKind::Other, PluginKind::Talkback]
        );
        assert_eq!(
            a.plugins.iter().map(|x| x.bypassed).collect::<Vec<_>>(),
            vec![false, true, false]
        );
        assert!(a.plugins[2].bypass_line.is_none());
        assert_eq!(a.plugins[1].name, "synthesis/tonegenerator");
        assert_eq!(p.body(&a.plugins[0]), vec!["3 0 0 -"]);
        let b = &p.tracks[1];
        assert_eq!(b.name, "OUT");
        assert_eq!((b.vol, b.pan, b.muted, b.soloed), (0.25, 0.5, true, true));
        assert_eq!(b.rec, Some((false, -1)));
        assert!(b.fx_on);
        assert_eq!(b.hwout, vec![2]);
        assert_eq!(b.receives.len(), 2);
        let r = &b.receives[0];
        assert_eq!(
            (r.src, r.mode, r.vol, r.pan, r.mute, r.plain),
            (0, 3, 0.5, -1.0, true, true)
        );
        assert!(!b.receives[1].plain);
        assert_eq!(
            b.plugins.iter().map(|x| x.kind).collect::<Vec<_>>(),
            vec![
                PluginKind::ReaEq,
                PluginKind::Limiter,
                PluginKind::Listen,
                PluginKind::Other
            ]
        );
        assert!(b.plugins[1].bypassed);
        assert_eq!(b.plugins[3].name, "PARMENV");
        let m = &p.master;
        assert_eq!((m.vol, m.pan, m.mute_flags), (0.5, -0.25, 1));
        assert_eq!(m.hwout, vec![0]);
        assert_eq!(m.active_fx, 0);
        assert_eq!(p.doc.content(m.volume), "MASTER_VOLUME 0.5 -0.25 -1 -1 1");
    }

    #[test]
    fn active_master_plugins_are_counted() {
        let text = TEXT.replace(
            "    BYPASS 1 0 0\n    <JS some",
            "    BYPASS 0 0 0\n    <JS some",
        );
        assert_eq!(LegacyProject::parse(&text).unwrap().master.active_fx, 1);
    }

    #[test]
    fn broken_projects_are_errors() {
        for (from, to) in [
            ("<REAPER_PROJECT", "<OTHER_PROJECT"),
            ("  MASTER_VOLUME 0.5 -0.25 -1 -1 1\n", ""),
            ("VOLPAN 0.25 0.5", "VOLPAN x 0.5"),
            ("VOLPAN 0.25 0.5", "VOLPAN inf 0.5"),
            ("MUTESOLO 1 1 1", "MUTESOLO 1"),
            ("AUXRECV 0 3", "AUXRECV -1 3"),
            ("REC 1 4", "REC 1 x"),
            ("FX 0", "FX"),
            ("HWOUT 2", "HWOUT z"),
            (
                "BYPASS 1 0 0\n      <JS loser",
                "BYPASS x 0 0\n      <JS loser",
            ),
            ("MASTERMUTESOLO 1", "MASTERMUTESOLO y"),
        ] {
            let text = TEXT.replacen(from, to, 1);
            assert_ne!(text, TEXT, "{from:?}");
            assert!(LegacyProject::parse(&text).is_err(), "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn plugins_are_classified_by_their_head() {
        fn s(v: &[&str]) -> Vec<String> {
            v.iter().map(|x| (*x).to_owned()).collect()
        }
        assert_eq!(
            classify(&s(&["JS", "utility/volume_pan", "x"])).0,
            PluginKind::Trim
        );
        assert_eq!(
            classify(&s(&["VST", "VST: ReaEQ (Cockos)"])).0,
            PluginKind::ReaEq
        );
        assert_eq!(
            classify(&s(&["JS", "VST: ReaEQ (Cockos)"])).0,
            PluginKind::Other
        );
        assert_eq!(
            classify(&s(&["VST", "utility/volume_pan"])).0,
            PluginKind::Other
        );
        assert_eq!(
            classify(&s(&["CLAP", "x VBAN IEM y"])).0,
            PluginKind::Listen
        );
        assert_eq!(
            classify(&s(&["PARMENV", "1"])),
            (PluginKind::Other, "PARMENV".to_owned())
        );
        assert_eq!(classify(&s(&["JS"])), (PluginKind::Other, "JS".to_owned()));
        assert_eq!(classify(&[]), (PluginKind::Other, String::new()));
    }
}
