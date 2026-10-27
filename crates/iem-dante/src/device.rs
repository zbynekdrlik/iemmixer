// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors. The 31-character device name
// limit is inferno's note (teodly/inferno at 9767558,
// inferno_aoip/src/device_info.rs, GPL-3.0-or-later OR AGPL-3.0-or-later),
// and so is the subscriptions' saved form (an RX channel's number with the
// TX channel's and device's names, inferno_aoip/src/device_server/
// channels_subscriber.rs `SavedChannelState`); the rest is iemmixer's own
// code.

//! The device's identity (#72 phase A): its config, a private TOML on the
//! box (this repository holds only the synthetic
//! `config/test-dante-device.toml`), and its state, the names Dante
//! Controller set, kept in a state file so a restart comes back with them.
//!
//! - The config's TX names are the channels' default names (ARC 0x2000, the
//!   mDNS record marked `default`); the state's are their current names
//!   (ARC 0x2010, and a second mDNS record while they differ). RX names
//!   exist only as the state's (ARC 0x3000).
//! - The state's `name` is the device name it lists. Phase A has no device
//!   rename, so it is the config's name unless a state file says otherwise.
//! - The state's RX subscriptions (phase B): which TX channel of which
//!   device each RX channel receives, set on our own device only (ARC
//!   0x3010), kept so a restart subscribes again. The file lists the
//!   subscribed channels by number (`[[subscriptions]]`, `rx` from 1); a
//!   state without any is written as phase A wrote it, so a phase A file
//!   loads unchanged.
//! - The state follows the config's channel counts (#72 phase C, Live 1: a
//!   config whose counts change is a deliberate act): a state file with
//!   fewer channels gets the new ones at the config's names, one with more
//!   is cut where nothing of its own would go ([`DeviceState::fit`]), and
//!   the fitted state is saved at once.
//! - Problems name a field and a channel number, never a name: the channel
//!   names may be the band's labels (P6), and no log may hold one.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::stream::{BLOCK, MAX_WORK_MARGIN, WORK_MARGIN};

/// The longest device or channel name, in bytes: Dante Controller ignores a
/// device with a longer name (inferno's `device_info.rs`), and Dante's
/// channel labels hold 31 characters too.
pub const MAX_NAME: usize = 31;
/// The most channels the device declares per direction.
pub const MAX_CHANNELS: usize = 512;
/// The sample rates the device can declare.
pub const SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// The encodings (bits per sample) the device can declare.
pub const ENCODINGS: [u8; 3] = [16, 24, 32];

/// The device as the box's config declares it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// The device name ([`valid_device_name`]); the factory name of ARC
    /// 0x1003.
    pub name: String,
    /// mDNS `mf`.
    pub manufacturer: String,
    /// The board name: ARC 0x1003 and mDNS `router_info`.
    pub model: String,
    /// The RX channels' names, in channel order (channel 1 first).
    pub rx: Vec<String>,
    /// The TX channels' default names, in channel order.
    pub tx: Vec<String>,
    /// Hz, one of [`SAMPLE_RATES`].
    pub sample_rate: u32,
    /// Bits per sample, one of [`ENCODINGS`].
    pub encoding: u8,
    /// The latency the channels advertise, in ns (1 to `u32::MAX`, the
    /// width Dante's flow descriptors give it).
    pub latency_ns: u64,
    /// W, the engine pump's work margin in samples: [`BLOCK`] to
    /// [`MAX_WORK_MARGIN`], [`WORK_MARGIN`] when the config leaves it out.
    #[serde(default = "default_work_margin")]
    pub work_margin: u64,
    /// Where the state ([`DeviceState`]) is kept.
    pub state_file: PathBuf,
}

/// Why a config was refused: every problem, in fields and channel numbers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("device config: {}", .problems.join("; "))]
pub struct ConfigError {
    pub problems: Vec<String>,
}

impl DeviceConfig {
    /// Parses and checks a config; every problem is listed together.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let cfg: DeviceConfig = toml::from_str(text).map_err(|e| ConfigError {
            problems: vec![parse_problem(text, &e)],
        })?;
        let problems = cfg.problems();
        if problems.is_empty() {
            Ok(cfg)
        } else {
            Err(ConfigError { problems })
        }
    }

    fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !valid_device_name(&self.name) {
            out.push(DEVICE_NAME_RULE.to_string());
        }
        for (field, value) in [("manufacturer", &self.manufacturer), ("model", &self.model)] {
            if !valid_label(value) {
                out.push(format!(
                    "{field}: 1 to {MAX_NAME} printable ASCII characters"
                ));
            }
        }
        for (dir, names) in [("RX", &self.rx), ("TX", &self.tx)] {
            if names.len() > MAX_CHANNELS {
                out.push(format!(
                    "{dir}: {} channels, at most {MAX_CHANNELS}",
                    names.len()
                ));
            }
            out.extend(channel_problems(dir, names, None));
        }
        if !SAMPLE_RATES.contains(&self.sample_rate) {
            out.push(format!(
                "sample_rate: {} Hz is not one of {SAMPLE_RATES:?}",
                self.sample_rate
            ));
        }
        if !ENCODINGS.contains(&self.encoding) {
            out.push(format!(
                "encoding: {} bits is not one of {ENCODINGS:?}",
                self.encoding
            ));
        }
        if self.latency_ns == 0 || self.latency_ns > u64::from(u32::MAX) {
            out.push(format!("latency_ns: 1 to {}", u32::MAX));
        }
        if !(BLOCK..=MAX_WORK_MARGIN).contains(&self.work_margin) {
            out.push(format!("work_margin: {BLOCK} to {MAX_WORK_MARGIN} samples"));
        }
        if self.state_file.as_os_str().is_empty() {
            out.push("state_file: empty".to_string());
        }
        out
    }
}

fn default_work_margin() -> u64 {
    WORK_MARGIN
}

const DEVICE_NAME_RULE: &str = "name: not a valid Dante device name \
     (1 to 31 of A-Z a-z 0-9 and '-', no '-' at either end)";

/// A Dante device name: 1 to [`MAX_NAME`] of A-Z, a-z, 0-9 and `-`, with no
/// `-` at either end (Dante Controller's rule; it is also the device's
/// mDNS host label).
pub fn valid_device_name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A Dante channel name: 1 to [`MAX_NAME`] printable ASCII characters
/// (space included), none of `=`, `.` and `@` (Dante's labels: `@` joins a
/// channel to its device, `=` splits a TXT entry, `.` a DNS name).
pub fn valid_channel_name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len())
        && s.bytes()
            .all(|b| (0x20..=0x7e).contains(&b) && !matches!(b, b'=' | b'.' | b'@'))
}

/// A manufacturer or model: 1 to [`MAX_NAME`] printable ASCII characters.
fn valid_label(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len()) && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// The problems of one direction's channel names, by channel number: an
/// invalid name, a name another channel has (case aside, as mDNS compares
/// names), and for TX names with their `defaults` a name that is another
/// channel's default name (it would be that channel's mDNS name too).
pub(crate) fn channel_problems(
    dir: &str,
    names: &[String],
    defaults: Option<&[String]>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut first: HashMap<String, usize> = HashMap::new();
    let default_of: HashMap<String, usize> = defaults
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, d)| (d.to_ascii_lowercase(), i))
        .collect();
    for (i, name) in names.iter().enumerate() {
        let n = i + 1;
        if !valid_channel_name(name) {
            out.push(format!(
                "{dir} channel {n}: not a valid Dante channel name \
                 (1 to 31 printable ASCII characters, none of = . @)"
            ));
            continue;
        }
        let key = name.to_ascii_lowercase();
        if let Some(&j) = first.get(&key) {
            out.push(format!(
                "{dir} channel {n}: the same name as channel {}",
                j + 1
            ));
        } else {
            first.insert(key.clone(), i);
        }
        if let Some(&j) = default_of.get(&key).filter(|&&j| j != i) {
            out.push(format!(
                "{dir} channel {n}: the default name of channel {}",
                j + 1
            ));
        }
    }
    out
}

/// The first channel past the config's `defaults` (its number, from 1)
/// whose name in `names` is not its default name ([`default_past`]):
/// a cut there would drop a rename.
fn renamed_past(names: &[String], defaults: &[String]) -> Option<usize> {
    (defaults.len()..names.len())
        .find(|&i| default_past(defaults, i + 1).as_deref() != Some(names[i].as_str()))
        .map(|i| i + 1)
}

/// Channel `n`'s default name past the config's `defaults` (`n` above
/// their count; the config names no such channel): the config's numbering
/// carried on, when its last name ends in its own channel number (`RX 32`
/// → `RX 33`; `064` → `065`, the digits' width kept); `None` when it does
/// not, or the config has no channel there: no default is known, so any
/// name past the count counts as a rename.
fn default_past(defaults: &[String], n: usize) -> Option<String> {
    let last = defaults.last()?;
    let prefix = last.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &last[prefix.len()..];
    let numbered = digits.parse::<usize>().ok()? == defaults.len();
    numbered.then(|| format!("{prefix}{n:0width$}", width = digits.len()))
}

/// The running device: its config and the names it lists now. The ARC
/// server changes the names; the mDNS records are built from both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub cfg: DeviceConfig,
    pub state: DeviceState,
}

impl Device {
    /// The device as it starts: `cfg`, with the names of its state file
    /// (the config's when there is none; one with other channel counts
    /// fitted to `cfg` and saved; [`DeviceState::load_or_default`]).
    pub fn load(cfg: DeviceConfig) -> io::Result<Self> {
        let state = DeviceState::load_or_default(&cfg)?;
        Ok(Self { cfg, state })
    }
}

/// An RX channel's subscription: the TX channel and the device it comes
/// from, by their Dante names (as Dante Controller sets them, ARC 0x3010).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub tx_channel: String,
    pub tx_device: String,
}

impl Subscription {
    /// Whether both names follow Dante's rules.
    pub fn is_valid(&self) -> bool {
        valid_channel_name(&self.tx_channel) && valid_device_name(&self.tx_device)
    }
}

/// The names the device lists now: the config's until Dante Controller
/// renames a channel, then the state file's; and its RX subscriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceState {
    pub name: String,
    pub rx: Vec<String>,
    pub tx: Vec<String>,
    /// RX channel `n`'s subscription at index `n − 1`: empty while none was
    /// ever made, else one entry per RX channel (anything else is refused).
    /// The file lists only the subscribed channels, by number.
    #[serde(default, with = "saved", skip_serializing_if = "saved::none")]
    pub subscriptions: Vec<Option<Subscription>>,
}

/// What [`DeviceState::fit`] changed: each direction's channel count
/// before and after (the config's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refit {
    pub rx: (usize, usize),
    pub tx: (usize, usize),
}

impl DeviceState {
    /// The config's names, no subscription.
    pub fn from_config(cfg: &DeviceConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            rx: cfg.rx.clone(),
            tx: cfg.tx.clone(),
            subscriptions: Vec::new(),
        }
    }

    /// RX channel `rx`'s subscription (from 0).
    pub fn subscription(&self, rx: usize) -> Option<&Subscription> {
        self.subscriptions.get(rx)?.as_ref()
    }

    /// Sets (or with `None` removes) RX channel `rx`'s subscription (from
    /// 0); `false`, nothing changed, for a channel `cfg` does not have or
    /// a name Dante's rules refuse. The list grows to one entry per RX
    /// channel at the first change.
    pub fn set_subscription(
        &mut self,
        cfg: &DeviceConfig,
        rx: usize,
        sub: Option<Subscription>,
    ) -> bool {
        if rx >= cfg.rx.len() || sub.as_ref().is_some_and(|s| !s.is_valid()) {
            return false;
        }
        self.subscriptions.resize(cfg.rx.len(), None);
        self.subscriptions[rx] = sub;
        true
    }

    /// The state file's names, or the config's when there is no file
    /// (nothing written then). A file with other channel counts than the
    /// config's is fitted to them ([`DeviceState::fit`]) and, once it holds,
    /// saved over the file at once, so the next start reads a matching one
    /// (one info line, the counts only). A file that cannot be read, cannot
    /// be fitted or does not hold is an error naming the file, the file
    /// left as it was; so is a fitted state that cannot be saved.
    pub fn load_or_default(cfg: &DeviceConfig) -> io::Result<Self> {
        let path = &cfg.state_file;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::from_config(cfg)),
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        let invalid = |why: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {why}", path.display()),
            )
        };
        let mut state: Self =
            toml::from_str(&text).map_err(|e| invalid(parse_problem(&text, &e)))?;
        // The file lists the subscribed channels up to the highest; the
        // state holds one entry per RX channel.
        if !state.subscriptions.is_empty() {
            let len = state.subscriptions.len().max(state.rx.len());
            state.subscriptions.resize(len, None);
        }
        let refit = state
            .fit(cfg)
            .map_err(|problems| invalid(problems.join("; ")))?;
        let problems = state.problems(cfg);
        if !problems.is_empty() {
            return Err(invalid(problems.join("; ")));
        }
        if let Some(refit) = refit {
            state.save(path)?;
            info!(
                rx_before = refit.rx.0,
                rx_after = refit.rx.1,
                tx_before = refit.tx.0,
                tx_after = refit.tx.1,
                "the device state follows the config's channel counts (saved)"
            );
        }
        Ok(state)
    }

    /// Fits the channel lists to `cfg`'s counts (#72 phase C, Live 1: a
    /// config whose counts change is a deliberate act). A direction with
    /// fewer channels than the config gets the config's names for the new
    /// ones (no subscription); its channels keep their names and
    /// subscriptions. One with more is cut to the config's count, but only
    /// when every channel cut off is at its default name
    /// ([`default_past`]) and, for RX, no subscription past the count is
    /// held: else nothing changes and the problems name the kind and the
    /// first channel by number (never a name). `None` when the counts
    /// already fit.
    pub fn fit(&mut self, cfg: &DeviceConfig) -> Result<Option<Refit>, Vec<String>> {
        let refit = Refit {
            rx: (self.rx.len(), cfg.rx.len()),
            tx: (self.tx.len(), cfg.tx.len()),
        };
        if refit.rx.0 == refit.rx.1 && refit.tx.0 == refit.tx.1 {
            return Ok(None);
        }
        let cuts_rx = refit.rx.0 > refit.rx.1;
        let mut out = Vec::new();
        for (dir, names, defaults) in [("RX", &self.rx, &cfg.rx), ("TX", &self.tx, &cfg.tx)] {
            if let Some(n) = renamed_past(names, defaults) {
                out.push(format!(
                    "{} {dir} channels, the config {}: \
                     a renamed {dir} channel past the config's count (channel {n})",
                    names.len(),
                    defaults.len()
                ));
            }
        }
        // A fit that does not cut RX leaves a subscription past the RX
        // count to `problems` (the file's own fault, refused as before).
        let subscribed = (self.subscriptions.iter().enumerate())
            .skip(cfg.rx.len())
            .find(|(_, sub)| sub.is_some());
        if let Some((i, _)) = subscribed.filter(|_| cuts_rx) {
            out.push(format!(
                "{} RX channels, the config {}: \
                 a subscribed RX channel past the config's count (channel {})",
                self.rx.len(),
                cfg.rx.len(),
                i + 1
            ));
        }
        if !out.is_empty() {
            return Err(out);
        }
        for (names, defaults) in [(&mut self.rx, &cfg.rx), (&mut self.tx, &cfg.tx)] {
            names.truncate(defaults.len());
            let kept = names.len();
            names.extend_from_slice(&defaults[kept..]);
        }
        // One subscription entry per RX channel, or none.
        if cuts_rx {
            self.subscriptions.truncate(cfg.rx.len());
        } else if !self.subscriptions.is_empty() {
            let len = self.subscriptions.len().max(cfg.rx.len());
            self.subscriptions.resize(len, None);
        }
        Ok(Some(refit))
    }

    /// Whether these names fit `cfg`: its channel counts, Dante's name
    /// rules, and no TX name that is another channel's (default) name; the
    /// subscriptions: none or one entry per RX channel, each with names
    /// Dante's rules allow.
    pub fn problems(&self, cfg: &DeviceConfig) -> Vec<String> {
        let mut out = Vec::new();
        if !valid_device_name(&self.name) {
            out.push(DEVICE_NAME_RULE.to_string());
        }
        for (dir, names, want) in [
            ("RX", &self.rx, cfg.rx.len()),
            ("TX", &self.tx, cfg.tx.len()),
        ] {
            if names.len() != want {
                out.push(format!("{} {dir} channels, the config {want}", names.len()));
            }
        }
        let (subs, want) = (self.subscriptions.len(), cfg.rx.len());
        if subs > want {
            out.push(format!(
                "a subscription of RX channel {subs}, the config has {want} RX channels"
            ));
        } else if subs != 0 && subs != want {
            out.push(format!(
                "{subs} subscription entries, the config has {want} RX channels"
            ));
        }
        if out.is_empty() {
            out.extend(channel_problems("RX", &self.rx, None));
            out.extend(channel_problems("TX", &self.tx, Some(&cfg.tx)));
            for (i, sub) in self.subscriptions.iter().enumerate() {
                let Some(sub) = sub else { continue };
                if !valid_channel_name(&sub.tx_channel) {
                    out.push(format!(
                        "RX channel {}: a subscription to an invalid channel name",
                        i + 1
                    ));
                }
                if !valid_device_name(&sub.tx_device) {
                    out.push(format!(
                        "RX channel {}: a subscription to an invalid device name",
                        i + 1
                    ));
                }
            }
        }
        out
    }

    /// Whether [`DeviceState::save`] can write `path`, asked at the daemon's
    /// start so a folder it cannot write is found then, not at the first
    /// rename: the temporary file `save` writes beside it is created and
    /// removed again. An error names the file; `path` itself is never
    /// touched.
    pub fn check_writable(path: &Path) -> io::Result<()> {
        let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let tmp = temporary(path).map_err(named)?;
        fs::File::create(&tmp)
            .and_then(|file| file.sync_all())
            .and_then(|()| fs::remove_file(&tmp))
            .map_err(named)
    }

    /// Writes the state to `path`: a synced temporary file beside it, then a
    /// rename over it and a sync of the directory, so a crash leaves the old
    /// state or the new one, never a torn file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let tmp = temporary(path).map_err(named)?;
        let text = toml::to_string(self).map_err(io::Error::other)?;
        let written = write_synced(&tmp, text.as_bytes())
            .and_then(|()| fs::rename(&tmp, path))
            .and_then(|()| sync_dir(path));
        if written.is_err() {
            // The temporary file, if any, is not the state: it goes.
            let _ = fs::remove_file(&tmp);
        }
        written.map_err(named)
    }
}

/// The subscriptions as the state file holds them: one `[[subscriptions]]`
/// table per subscribed RX channel, `rx` its number from 1 (inferno's
/// `SavedChannelState`), read back as one entry per channel up to the
/// highest listed. A number of 0, past [`MAX_CHANNELS`] or listed twice is
/// refused by its number (never a name).
mod saved {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    use super::{MAX_CHANNELS, Subscription};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        rx: u32,
        tx_channel: String,
        tx_device: String,
    }

    /// No subscription at all: the field is left out.
    pub(super) fn none(subs: &[Option<Subscription>]) -> bool {
        subs.iter().all(Option::is_none)
    }

    pub(super) fn serialize<S: Serializer>(
        subs: &[Option<Subscription>],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let entries: Vec<Entry> = subs
            .iter()
            .enumerate()
            .filter_map(|(i, sub)| {
                let sub = sub.as_ref()?;
                Some(Entry {
                    rx: u32::try_from(i + 1).ok()?,
                    tx_channel: sub.tx_channel.clone(),
                    tx_device: sub.tx_device.clone(),
                })
            })
            .collect();
        entries.serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<Option<Subscription>>, D::Error> {
        let entries = Vec::<Entry>::deserialize(d)?;
        let mut subs: Vec<Option<Subscription>> = Vec::new();
        for e in entries {
            let rx = usize::try_from(e.rx).unwrap_or(usize::MAX);
            if rx == 0 || rx > MAX_CHANNELS {
                return Err(D::Error::custom(format!(
                    "RX channel {}: no device has it (1 to {MAX_CHANNELS})",
                    e.rx
                )));
            }
            subs.resize(subs.len().max(rx), None);
            if subs[rx - 1].is_some() {
                return Err(D::Error::custom(format!(
                    "RX channel {rx} is subscribed twice"
                )));
            }
            subs[rx - 1] = Some(Subscription {
                tx_channel: e.tx_channel,
                tx_device: e.tx_device,
            });
        }
        Ok(subs)
    }
}

/// The temporary file [`DeviceState::save`] writes beside `path`
/// (`<name>.tmp`); an error for a path that is no file path.
fn temporary(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    Ok(path.with_file_name(tmp_name))
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Syncs the directory holding `path`, so the rename is durable (Unix; on
/// Windows a directory cannot be opened as a file and the rename is
/// durable through NTFS's journal).
#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// A parse error as a line number and the message with every quoted span
/// dropped ([`redact`]): toml's own rendering shows the line, and neither it
/// nor a quoted value (a name, a misspelt key) may reach a log.
fn parse_problem(text: &str, e: &toml::de::Error) -> String {
    let message = redact(e.message());
    match e.span() {
        Some(span) => {
            let end = span.start.min(text.len());
            let line = text.as_bytes()[..end]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1;
            format!("line {line}: {message}")
        }
        None => message,
    }
}

/// `message` with every quoted span, `"..."` or `` `...` ``, cut to its
/// quote marks around an ellipsis; inside a span a backslash escapes the next
/// character, and an unclosed span runs to the end. serde quotes the
/// offending value and toml backticks a key.
fn redact(message: &str) -> String {
    let mut out = String::new();
    let mut open: Option<char> = None;
    let mut escaped = false;
    for c in message.chars() {
        match open {
            None if c == '"' || c == '`' => {
                out.push(c);
                out.push('\u{2026}');
                out.push(c);
                open = Some(c);
            }
            None => out.push(c),
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => open = None,
            Some(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests;
