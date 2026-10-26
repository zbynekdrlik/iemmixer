// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 iemmixer contributors. The 31-character device name
// limit is inferno's note (teodly/inferno at 9767558,
// inferno_aoip/src/device_info.rs, GPL-3.0-or-later OR AGPL-3.0-or-later);
// the rest is iemmixer's own code.

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
//! - Problems name a field and a channel number, never a name: the channel
//!   names may be the band's labels (P6), and no log may hold one.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
        if self.state_file.as_os_str().is_empty() {
            out.push("state_file: empty".to_string());
        }
        out
    }
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

/// The names the device lists now: the config's until Dante Controller
/// renames a channel, then the state file's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceState {
    pub name: String,
    pub rx: Vec<String>,
    pub tx: Vec<String>,
}

impl DeviceState {
    /// The config's names.
    pub fn from_config(cfg: &DeviceConfig) -> Self {
        Self {
            name: cfg.name.clone(),
            rx: cfg.rx.clone(),
            tx: cfg.tx.clone(),
        }
    }

    /// The state file's names, or the config's when there is no file. A file
    /// that cannot be read or does not fit the config is an error naming
    /// the file: never silently cut or padded.
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
        let state: Self = toml::from_str(&text).map_err(|e| invalid(parse_problem(&text, &e)))?;
        let problems = state.problems(cfg);
        if problems.is_empty() {
            Ok(state)
        } else {
            Err(invalid(problems.join("; ")))
        }
    }

    /// Whether these names fit `cfg`: its channel counts, Dante's name
    /// rules, and no TX name that is another channel's (default) name.
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
        if out.is_empty() {
            out.extend(channel_problems("RX", &self.rx, None));
            out.extend(channel_problems("TX", &self.tx, Some(&cfg.tx)));
        }
        out
    }

    /// Writes the state to `path`: a synced temporary file beside it, then a
    /// rename over it and a sync of the directory, so a crash leaves the old
    /// state or the new one, never a torn file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let named = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        let file_name = path.file_name().ok_or_else(|| {
            named(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a file path",
            ))
        })?;
        let mut tmp_name = file_name.to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
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
