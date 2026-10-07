//! The soak harness (S7 design note §4): logs in as the engineer through the
//! server's login, opens one mixer socket and one listen socket on one
//! member's mix at the LAN address the server names (`/api/site`), decodes
//! the Opus frames and counts frames, gaps and meter frames into a JSON
//! summary. It reads only: it sends no mixer command. It never opens a
//! socket twice (#10): a lost socket ends the run. The PIN comes from
//! `IEM_SOAK_PIN`, never from the command line. Nothing here ends a
//! process: each socket ends with a WebSocket Close.
//!
//! This file is the pure core: the arguments, the URLs, the gap clock, the
//! event classes and the summary. [`tally`] counts a run into its summary
//! (pure too); [`net`] is the wire: the login and the socket threads.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod net;
pub mod tally;

/// More than this without a listen frame is a gap.
pub const GAP: Duration = Duration::from_millis(60);
/// One Opus frame: 20 ms, 960 samples per channel at 48 kHz, stereo (the
/// server's listen encoder, X4).
pub const FRAME: Duration = Duration::from_millis(20);
/// The summary's schema.
pub const SCHEMA: u32 = 1;
/// The environment variable that holds the engineer's PIN.
pub const PIN_ENV: &str = "IEM_SOAK_PIN";
/// The build, as the guard names its own (`GITHUB_SHA` in CI).
pub const BUILD: &str = match option_env!("GITHUB_SHA") {
    Some(sha) => sha,
    None => "local",
};
/// The UI protocol the mixer socket speaks (`iem_core::ws::UI_PROTO`; the
/// tests check that the server serves it).
pub const UI_PROTO: u16 = 2;
/// `--seconds` at most: 10 h (the soak job's 600 min).
pub const MAX_SECONDS: u64 = 36_000;
/// Where `/api/site` is read without `--base`: the server on this PC.
pub const DEFAULT_BASE: &str = "http://127.0.0.1";

pub const USAGE: &str = "\
iem-soakclient --member ID --seconds N --out FILE [--base URL] [--direct] [--cpu-sets IDS]

Logs in as the engineer (the PIN from IEM_SOAK_PIN, never an argument), opens
one mixer socket and one listen socket on the mix of member ID at the LAN
address the server at --base names (/api/site; with --direct at --base
itself), and writes a JSON summary to FILE every minute and at the end. It
reads only.

  --seconds N     1 to 36000
  --base URL      http://HOST[:PORT], default http://127.0.0.1
  --cpu-sets IDS  CPU Set ids, e.g. 256,257 (Windows only)";

/// The command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// `--base`: where `/api/site` is read, as its origin `http://HOST[:PORT]`.
    pub base: String,
    /// `--direct`: everything through `base` (CI; the test site's `lan_url`
    /// is a placeholder).
    pub direct: bool,
    /// `--member`: the mix both sockets are on.
    pub member: String,
    /// `--seconds`, 1 to [`MAX_SECONDS`].
    pub seconds: u64,
    /// `--out`: the summary file.
    pub out: PathBuf,
    /// `--cpu-sets 256,257` (Windows): CPU Set ids, as the engine's
    /// `[card] cpu_sets`.
    pub cpu_sets: Vec<u32>,
}

/// Reads the arguments after the program's name. `Err` is the usage error;
/// it never repeats an argument's value (P6, a PIN typed by mistake).
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut base = None;
    let mut direct = false;
    let mut member = None;
    let mut seconds = None;
    let mut out = None;
    let mut cpu_sets = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let slot: &mut Option<String> = match flag.as_str() {
            "--direct" => {
                direct = true;
                continue;
            }
            "--base" => &mut base,
            "--member" => &mut member,
            "--seconds" => &mut seconds,
            "--out" => &mut out,
            "--cpu-sets" => &mut cpu_sets,
            f if f == "--pin" || f.starts_with("--pin=") => {
                return Err(format!(
                    "the PIN comes from {PIN_ENV}, never from the command line"
                ));
            }
            _ => return Err("unknown argument (see the usage)".to_owned()),
        };
        if slot.is_some() {
            return Err(format!("{flag} is given twice"));
        }
        match it.next() {
            Some(value) if !value.starts_with("--") => *slot = Some(value.clone()),
            _ => return Err(format!("{flag} needs a value")),
        }
    }
    let member = member.ok_or("--member is required")?;
    if !valid_member(&member) {
        return Err("--member must be 1 to 64 letters, digits, '_' or '-'".to_owned());
    }
    let seconds = seconds
        .ok_or("--seconds is required")?
        .parse::<u64>()
        .ok()
        .filter(|s| (1..=MAX_SECONDS).contains(s))
        .ok_or_else(|| format!("--seconds must be a whole number from 1 to {MAX_SECONDS}"))?;
    let out = out.filter(|o| !o.is_empty()).ok_or("--out is required")?;
    let base = http_origin(base.as_deref().unwrap_or(DEFAULT_BASE))
        .ok_or("--base must be http://HOST[:PORT]")?;
    let cpu_sets = match cpu_sets {
        None => Vec::new(),
        Some(list) => {
            let ids = parse_cpu_sets(&list).ok_or("--cpu-sets takes CPU Set ids, e.g. 256,257")?;
            if !cfg!(windows) {
                return Err("--cpu-sets is Windows only".to_owned());
            }
            ids
        }
    };
    Ok(Args {
        base,
        direct,
        member,
        seconds,
        out: PathBuf::from(out),
        cpu_sets,
    })
}

/// A member id as the site writes it: 1 to 64 of `[A-Za-z0-9_-]`.
fn valid_member(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `256,257`: CPU Set ids; anything else is `None`.
fn parse_cpu_sets(list: &str) -> Option<Vec<u32>> {
    list.split(',').map(|id| id.parse().ok()).collect()
}

/// The engineer's PIN from [`PIN_ENV`]: 4 to 12 digits. The error never
/// repeats the value.
pub fn pin_from(value: Option<String>) -> Result<String, String> {
    match value {
        Some(pin) if (4..=12).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(pin)
        }
        Some(_) => Err(format!(
            "{PIN_ENV} must hold the engineer PIN: 4 to 12 digits"
        )),
        None => Err(format!("{PIN_ENV} is not set")),
    }
}

/// `http://HOST[:PORT]` of an `http://` URL (the scheme in any case; a
/// path, query or fragment dropped); `None` for any other URL.
fn http_origin(url: &str) -> Option<String> {
    let scheme = url.get(..7)?;
    if !scheme.eq_ignore_ascii_case("http://") {
        return None;
    }
    let host = url.get(7..)?.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("http://{host}"))
}

/// Where the sockets and the login go: the server's own LAN address
/// (`/api/site`'s `lan_url`), or `--base` itself with `--direct`.
pub fn origin(args: &Args, lan_url: Option<&str>) -> Result<String, Reason> {
    if args.direct {
        return Ok(args.base.clone());
    }
    http_origin(lan_url.ok_or(Reason::SiteUnreadable)?).ok_or(Reason::NotHttp)
}

/// The `ws://` URL of `path` (with its query) at an `http://` origin.
pub fn ws_url(origin: &str, path: &str) -> String {
    let host = origin.strip_prefix("http://").unwrap_or(origin);
    format!("ws://{host}{path}")
}

/// The mixer socket of `member`'s page. The token is a JWT (URL-safe).
pub fn mixer_path(member: &str, token: &str) -> String {
    format!("/ws/{member}?token={token}&proto={UI_PROTO}")
}

/// The listen socket (engineer only).
pub fn listen_path(token: &str) -> String {
    format!("/ws/audio?token={token}")
}

/// The listen socket's start on `member`'s mix (`ClientMsg::ListenStart`).
pub fn listen_start(member: &str) -> String {
    serde_json::json!({"cmd": "ListenStart", "member_id": member}).to_string()
}

/// Milliseconds of `d`.
pub fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Gaps of the listen stream counted so far ([`Gaps::end`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GapCount {
    pub gaps: u64,
    /// The longest wait without a frame, a gap or not.
    pub max_gap_ms: u64,
}

/// The listen stream's clock: a wait of more than [`GAP`] between two frames,
/// or from the last frame to the end, is a gap.
#[derive(Debug, Clone, Default)]
pub struct Gaps {
    first: Option<Instant>,
    last: Option<Instant>,
    gaps: u64,
    longest: Duration,
}

impl Gaps {
    /// A frame arrived at `now`.
    pub fn frame(&mut self, now: Instant) {
        match self.last {
            Some(last) => {
                let wait = now.saturating_duration_since(last);
                if wait > GAP {
                    self.gaps += 1;
                }
                self.longest = self.longest.max(wait);
            }
            None => self.first = Some(now),
        }
        self.last = Some(now);
    }

    /// When the first frame arrived.
    pub fn first(&self) -> Option<Instant> {
        self.first
    }

    /// The count if the run ended at `now`: the wait since the last frame
    /// counts too; a run without a frame is one gap as long as the run
    /// since `started`. It changes nothing (the summary is written every
    /// minute).
    pub fn end(&self, started: Instant, now: Instant) -> GapCount {
        let (gaps, longest) = match self.last {
            Some(last) => {
                let wait = now.saturating_duration_since(last);
                (self.gaps + u64::from(wait > GAP), self.longest.max(wait))
            }
            None => (1, now.saturating_duration_since(started)),
        };
        GapCount {
            gaps,
            max_gap_ms: ms(longest),
        }
    }

    /// Frames a full stream would have sent by `now`: one per [`FRAME`] from
    /// the first frame on, that one included (none before it).
    pub fn expected(&self, now: Instant) -> u64 {
        self.first.map_or(0, |first| {
            let periods = now.saturating_duration_since(first).as_nanos() / FRAME.as_nanos();
            u64::try_from(periods).unwrap_or(u64::MAX).saturating_add(1)
        })
    }
}

/// What a text frame from the server is, for the counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A `Meters` event on the mixer socket.
    Meters,
    /// An `AudioStatus` on the listen socket, with its status
    /// (`listening`, `no_source`, `stopped`); its target is dropped.
    AudioStatus(String),
    /// Anything else, unreadable text included.
    Other,
}

/// A server event's tag (`iem_core::ws::ServerMsg`: `{"event": …, "data":
/// …}`); its data is skipped unread, whatever its shape.
#[derive(Deserialize)]
struct Tag {
    event: String,
}

/// An `AudioStatus` event's status, read only once the tag says so.
#[derive(Deserialize)]
struct Status {
    data: StatusData,
}

#[derive(Deserialize)]
struct StatusData {
    status: String,
}

/// The class of a text frame from the server.
pub fn classify(text: &str) -> Event {
    let Ok(Tag { event }) = serde_json::from_str::<Tag>(text) else {
        return Event::Other;
    };
    match event.as_str() {
        "Meters" => Event::Meters,
        "AudioStatus" => serde_json::from_str::<Status>(text)
            .map_or(Event::Other, |s| Event::AudioStatus(s.data.status)),
        _ => Event::Other,
    }
}

/// Why a run ended early: a fixed code, never a site value (P6). It is
/// serialised as its [`Reason::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// `/api/site` could not be read, or names no LAN URL.
    SiteUnreadable,
    /// The LAN URL is not plain `http://` (the client speaks no TLS).
    NotHttp,
    /// The login was refused (the PIN, or the login protection).
    LoginRefused,
    /// The login was not the engineer's (the listen socket is engineer-only).
    NotEngineer,
    /// The login got no answer, or a socket could not be opened (there is
    /// no second try).
    ServerGone,
    /// A socket closed, failed or stayed silent for the idle bound before
    /// the end: no socket is opened twice (#10).
    ConnectionLost,
    /// The process could not be placed on the given CPU Sets.
    CpuSets,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::SiteUnreadable => "site-unreadable",
            Reason::NotHttp => "not-http",
            Reason::LoginRefused => "login-refused",
            Reason::NotEngineer => "not-engineer",
            Reason::ServerGone => "server-gone",
            Reason::ConnectionLost => "connection-lost",
            Reason::CpuSets => "cpu-sets",
        }
    }
}

/// The harness's summary (S7 design note §4), rewritten every minute and at
/// the end: numbers and reason codes only (P6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub schema: u32,
    pub build: String,
    /// The whole `--seconds` ran.
    pub complete: bool,
    /// Since the sockets were first opened.
    pub seconds: f64,
    pub frames: u64,
    /// One per 20 ms from the first frame to the end.
    pub expected_frames: u64,
    /// Frames Opus refused, or that held other than 960 samples.
    pub decode_errors: u64,
    /// More than 60 ms without a frame (first frame to the end; a run
    /// without one is one gap).
    pub gaps: u64,
    /// The longest wait without a frame, a gap or not.
    pub max_gap_ms: u64,
    /// ListenStart to the first frame.
    pub first_frame_ms: Option<u64>,
    /// `Meters` events on the mixer socket.
    pub meter_frames: u64,
    /// Sockets opened again after a close: always 0, since no socket is
    /// opened twice (#10). Kept so the summary's schema 1 and the verdict's
    /// check 11 stay as they are.
    pub reconnects: u64,
    /// `AudioStatus` `no_source` answers.
    pub no_source: u64,
    /// Why the run ended early, as its [`Reason::code`]: a reason by type,
    /// so no error text (a URL, a host) can reach the summary (P6).
    pub error: Option<Reason>,
}

impl Default for Summary {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            build: BUILD.to_owned(),
            complete: false,
            seconds: 0.0,
            frames: 0,
            expected_frames: 0,
            decode_errors: 0,
            gaps: 0,
            max_gap_ms: 0,
            first_frame_ms: None,
            meter_frames: 0,
            reconnects: 0,
            no_source: 0,
            error: None,
        }
    }
}

/// Writes `summary` to `<path>.tmp`, flushes it to disk, then renames it
/// over `path` (as the guard's `state::write_atomic`), so a reader sees the
/// last whole summary, never a part. A failed rename leaves the `.tmp`,
/// which the next write replaces.
pub fn write_summary(path: &Path, summary: &Summary) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut text = serde_json::to_vec_pretty(summary).map_err(io::Error::other)?;
    text.push(b'\n');
    let mut file = File::create(&tmp)?;
    file.write_all(&text)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        let list: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
        parse_args(&list)
    }

    /// The three required flags and nothing else.
    const MINIMAL: [&str; 6] = ["--member", "member9", "--seconds", "600", "--out", "s.json"];

    /// [`MINIMAL`] followed by `extra`.
    fn with(extra: &[&str]) -> Result<Args, String> {
        let mut list = MINIMAL.to_vec();
        list.extend_from_slice(extra);
        args(&list)
    }

    /// [`MINIMAL`] with the value of `flag` replaced by `v`.
    fn replaced(flag: &str, v: &str) -> Result<Args, String> {
        let mut list = MINIMAL.to_vec();
        let at = list.iter().position(|a| *a == flag).unwrap();
        list[at + 1] = v;
        args(&list)
    }

    /// [`MINIMAL`] without `flag` and its value.
    fn without(flag: &str) -> Result<Args, String> {
        let mut list = MINIMAL.to_vec();
        let at = list.iter().position(|a| *a == flag).unwrap();
        list.remove(at);
        list.remove(at);
        args(&list)
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn count(gaps: u64, max_gap_ms: u64) -> GapCount {
        GapCount { gaps, max_gap_ms }
    }

    /// `origin` of [`MINIMAL`] (not `--direct`) for the server's `lan_url`.
    fn lan(url: Option<&str>) -> Result<String, Reason> {
        origin(&args(&MINIMAL).unwrap(), url)
    }

    #[test]
    fn the_arguments_and_their_defaults() {
        assert_eq!(
            args(&MINIMAL).unwrap(),
            Args {
                base: "http://127.0.0.1".to_owned(),
                direct: false,
                member: "member9".to_owned(),
                seconds: 600,
                out: PathBuf::from("s.json"),
                cpu_sets: Vec::new(),
            }
        );
        // Every flag, in any order; the base is kept as its origin.
        let all = args(&[
            "--out",
            "s.json",
            "--direct",
            "--seconds",
            "1",
            "--base",
            "http://127.0.0.1:8080/",
            "--member",
            "member9",
        ])
        .unwrap();
        assert_eq!(all.base, "http://127.0.0.1:8080");
        assert!(all.direct);
        assert_eq!(all.member, "member9");
        assert_eq!(all.seconds, 1);
        assert_eq!(all.out, PathBuf::from("s.json"));
        assert_eq!(replaced("--seconds", "36000").unwrap().seconds, MAX_SECONDS);
    }

    #[test]
    fn every_bad_argument_is_a_usage_error() {
        for flag in ["--member", "--seconds", "--out"] {
            assert!(without(flag).unwrap_err().contains(flag), "{flag}");
        }
        for seconds in ["0", "36001", "x", "-1", ""] {
            assert!(replaced("--seconds", seconds).is_err(), "{seconds:?}");
        }
        assert!(replaced("--out", "").is_err());
        // A flag without its value, and a flag taken as a value.
        assert!(args(&["--member", "member9", "--seconds", "600", "--out"]).is_err());
        assert!(replaced("--member", "--direct").is_err());
        // A flag given twice.
        assert!(with(&["--member", "member8"]).is_err());
        assert!(with(&["--seconds", "600"]).is_err());
        assert!(with(&["--bogus"]).unwrap_err().contains("unknown argument"));
        assert!(with(&["positional"]).is_err());
        // The base is plain HTTP with a host.
        assert!(with(&["--base", "https://mixer.example.org"]).is_err());
        assert!(with(&["--base", "http://"]).is_err());
        assert!(with(&["--cpu-sets", "256,x"]).is_err());
        assert!(with(&["--cpu-sets", ""]).is_err());
    }

    #[test]
    fn the_member_is_1_to_64_letters_digits_underscores_or_hyphens() {
        let longest = "m".repeat(64);
        for good in ["m", "member9", "Member_9-x", "_", "-", longest.as_str()] {
            assert_eq!(replaced("--member", good).unwrap().member, good);
        }
        let too_long = "m".repeat(65);
        for bad in [
            "",
            "member.9",
            "member 9",
            "member/9",
            "m?",
            too_long.as_str(),
        ] {
            assert!(replaced("--member", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_pin_never_comes_from_argv() {
        for extra in [&["--pin", "4321"][..], &["--pin=4321"][..]] {
            let e = with(extra).unwrap_err();
            assert!(e.contains(PIN_ENV), "{e}");
            assert!(!e.contains("4321"), "the value is never echoed: {e}");
        }
        // Another unknown flag gets the plain refusal, its value never echoed.
        let e = with(&["--bogus=4321"]).unwrap_err();
        assert!(!e.contains(PIN_ENV), "{e}");
        assert!(!e.contains("4321"), "{e}");
    }

    #[test]
    fn cpu_set_lists_are_comma_separated_ids() {
        assert_eq!(parse_cpu_sets("256,257"), Some(vec![256, 257]));
        assert_eq!(parse_cpu_sets("256"), Some(vec![256]));
        for bad in ["", "256,", ",256", "256,x", "256 257", "-1", "4294967296"] {
            assert_eq!(parse_cpu_sets(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn cpu_sets_are_windows_only() {
        let parsed = with(&["--cpu-sets", "256,257"]);
        if cfg!(windows) {
            assert_eq!(parsed.unwrap().cpu_sets, vec![256, 257]);
        } else {
            assert!(parsed.unwrap_err().contains("Windows only"));
        }
        // A bad list is refused on every OS.
        let bad = with(&["--cpu-sets", "256,x"]).unwrap_err();
        assert!(bad.contains("CPU Set ids"), "{bad}");
    }

    #[test]
    fn the_pin_comes_from_the_environment_as_4_to_12_digits() {
        for good in ["1234", "123456789012"] {
            assert_eq!(pin_from(Some(good.to_owned())), Ok(good.to_owned()));
        }
    }

    #[test]
    fn a_bad_pin_is_refused_without_echoing_it() {
        assert!(pin_from(None).unwrap_err().contains(PIN_ENV));
        for bad in ["", "123", "1234567890123", "12a4", "12 34", "+123", "abcd"] {
            let e = pin_from(Some(bad.to_owned())).unwrap_err();
            assert!(e.contains(PIN_ENV), "{e}");
            assert!(bad.is_empty() || !e.contains(bad), "never echoed: {e}");
        }
    }

    #[test]
    fn the_origin_is_the_named_lan_url_or_with_direct_the_base() {
        let ok = |s: &str| Ok(s.to_owned());
        assert_eq!(lan(Some("http://10.0.0.10/")), ok("http://10.0.0.10"));
        assert_eq!(
            lan(Some("http://10.0.0.10:8080")),
            ok("http://10.0.0.10:8080")
        );
        assert_eq!(lan(Some("HTTP://10.0.0.10/x?y#z")), ok("http://10.0.0.10"));
        assert_eq!(lan(None), Err(Reason::SiteUnreadable));
        for url in [
            "https://mixer.example.org",
            "http://",
            "http:///x",
            "10.0.0.10",
            "",
        ] {
            assert_eq!(lan(Some(url)), Err(Reason::NotHttp), "{url:?}");
        }
        let direct = with(&["--direct"]).unwrap();
        assert_eq!(
            origin(&direct, Some("http://10.0.0.10")),
            ok("http://127.0.0.1")
        );
        assert_eq!(origin(&direct, None), ok("http://127.0.0.1"));
    }

    #[test]
    fn the_socket_urls_and_the_listen_start() {
        let audio = ws_url("http://10.0.0.10:8080", "/ws/audio?token=t");
        assert_eq!(audio, "ws://10.0.0.10:8080/ws/audio?token=t");
        assert_eq!(mixer_path("member9", "t"), "/ws/member9?token=t&proto=2");
        assert_eq!(listen_path("t"), "/ws/audio?token=t");
        let start = listen_start("member9");
        assert_eq!(start, r#"{"cmd":"ListenStart","member_id":"member9"}"#);
    }

    #[test]
    fn a_wait_of_exactly_60_ms_is_no_gap_and_61_is() {
        let t0 = Instant::now();
        let mut g = Gaps::default();
        g.frame(t0);
        g.frame(at(t0, 60));
        assert_eq!(g.end(t0, at(t0, 60)), count(0, 60));
        g.frame(at(t0, 121));
        assert_eq!(g.end(t0, at(t0, 121)), count(1, 61));
        g.frame(at(t0, 141));
        assert_eq!(g.end(t0, at(t0, 141)), count(1, 61));
    }

    #[test]
    fn the_end_counts_the_wait_since_the_last_frame() {
        let t0 = Instant::now();
        let mut g = Gaps::default();
        g.frame(t0);
        g.frame(at(t0, 100));
        assert_eq!(g.end(t0, at(t0, 160)), count(1, 100));
        assert_eq!(g.end(t0, at(t0, 161)), count(2, 100));
        assert_eq!(g.end(t0, at(t0, 400)), count(2, 300));
        // `end` only reads: the frame that ends the wait counts it once.
        g.frame(at(t0, 400));
        assert_eq!(g.end(t0, at(t0, 400)), count(2, 300));
    }

    #[test]
    fn a_run_without_a_frame_is_one_gap_as_long_as_the_run() {
        let t0 = Instant::now();
        let g = Gaps::default();
        assert_eq!(g.end(t0, at(t0, 3_000)), count(1, 3_000));
        assert_eq!(g.end(t0, at(t0, 20)), count(1, 20));
        assert_eq!(g.expected(at(t0, 3_000)), 0);
        assert_eq!(g.first(), None);
    }

    #[test]
    fn a_long_wait_is_one_gap_and_the_first_frame_stays() {
        // The stream stalled after the frame at 0 ms and its next frame came
        // at 500 ms: one 500 ms gap, and the first frame stays the first.
        let t0 = Instant::now();
        let mut g = Gaps::default();
        g.frame(t0);
        g.frame(at(t0, 500));
        g.frame(at(t0, 520));
        assert_eq!(g.end(t0, at(t0, 520)), count(1, 500));
        assert_eq!(g.first(), Some(t0));
        assert_eq!(g.expected(at(t0, 520)), 27);
    }

    #[test]
    fn expected_frames_count_one_per_20_ms_from_the_first_frame() {
        let t0 = Instant::now();
        let mut g = Gaps::default();
        assert_eq!(g.expected(at(t0, 1_000)), 0);
        g.frame(at(t0, 100));
        assert_eq!(g.first(), Some(at(t0, 100)));
        assert_eq!(g.expected(at(t0, 100)), 1);
        assert_eq!(g.expected(at(t0, 119)), 1);
        assert_eq!(g.expected(at(t0, 120)), 2);
        assert_eq!(g.expected(at(t0, 1_100)), 51);
        assert_eq!(g.expected(at(t0, 100 + 8 * 3_600_000)), 1_440_001);
    }

    #[test]
    fn events_are_meters_audio_status_or_other() {
        let meters = r#"{"event":"Meters","data":{"meters":{"mic1":[0.1,0.1]}}}"#;
        assert_eq!(classify(meters), Event::Meters);
        for (text, status) in [
            (
                r#"{"event":"AudioStatus","data":{"status":"no_source"}}"#,
                "no_source",
            ),
            (
                r#"{"event":"AudioStatus","data":{"status":"listening","target":"member9"}}"#,
                "listening",
            ),
        ] {
            assert_eq!(classify(text), Event::AudioStatus(status.to_owned()));
        }
        for other in [
            r#"{"event":"Hello","data":{"proto":2,"build":"local","min_client_proto":2}}"#,
            r#"{"event":"State","data":{"channels":[],"connected":true}}"#,
            r#"{"event":"AudioStatus","data":{}}"#,
            r#"{"event":"meters","data":{"meters":{}}}"#,
            r#"{"data":{"status":"no_source"}}"#,
            "not json",
            "",
        ] {
            assert_eq!(classify(other), Event::Other, "{other}");
        }
    }

    #[test]
    fn a_meters_frame_of_any_data_shape_is_a_meter_frame() {
        // The tag decides: a field of another shape in a Meters frame's
        // data (here a numeric `status`) never makes the frame unreadable.
        for meters in [
            r#"{"event":"Meters","data":{"status":1,"meters":{}}}"#,
            r#"{"event":"Meters","data":[1]}"#,
            r#"{"event":"Meters"}"#,
        ] {
            assert_eq!(classify(meters), Event::Meters, "{meters}");
        }
        let numeric = r#"{"event":"AudioStatus","data":{"status":1}}"#;
        assert_eq!(classify(numeric), Event::Other);
    }

    #[test]
    fn the_summary_names_its_error_by_the_reason_code() {
        use Reason::*;
        for reason in [
            SiteUnreadable,
            NotHttp,
            LoginRefused,
            NotEngineer,
            ServerGone,
            ConnectionLost,
            CpuSets,
        ] {
            let summary = Summary {
                error: Some(reason),
                ..Summary::default()
            };
            let v = serde_json::to_value(&summary).unwrap();
            assert_eq!(v["error"], reason.code());
            let back: Summary = serde_json::from_value(v).unwrap();
            assert_eq!(back.error, Some(reason));
        }
        // Any other text is no reason (P6: the summary carries no free text).
        let free = r#"{"error":"http://10.0.0.10 refused"}"#;
        assert!(serde_json::from_str::<Summary>(free).is_err());
    }

    #[test]
    fn the_wire_forms_are_the_servers_own() {
        use iem_core::{ClientMsg, ServerMsg, tunnel::SiteLinks};
        // The mixer socket's protocol is one the server serves.
        let served = iem_core::MIN_CLIENT_PROTO..=iem_core::UI_PROTO;
        assert!(served.contains(&UI_PROTO));
        let start = ClientMsg::ListenStart {
            member_id: "member9".to_owned(),
        };
        assert_eq!(
            listen_start("member9"),
            serde_json::to_string(&start).unwrap()
        );
        let wire = |m: &ServerMsg| serde_json::to_string(m).unwrap();
        let meters = ServerMsg::Meters {
            meters: [("mic1".to_owned(), [0.1, 0.1])].into(),
        };
        assert_eq!(classify(&wire(&meters)), Event::Meters);
        let status = ServerMsg::AudioStatus {
            status: "no_source".to_owned(),
            target: Some("member9".to_owned()),
        };
        assert_eq!(
            classify(&wire(&status)),
            Event::AudioStatus("no_source".to_owned())
        );
        let hello = ServerMsg::Hello {
            proto: iem_core::UI_PROTO,
            build: "local".to_owned(),
            min_client_proto: iem_core::MIN_CLIENT_PROTO,
        };
        assert_eq!(classify(&wire(&hello)), Event::Other);
        // `/api/site`'s body names the LAN URL `lan_url`.
        let site = serde_json::to_value(SiteLinks {
            lan_url: Some("http://10.0.0.10/".to_owned()),
            public_host: Some("mixer.example.org".to_owned()),
        })
        .unwrap();
        assert_eq!(
            lan(site["lan_url"].as_str()),
            Ok("http://10.0.0.10".to_owned())
        );
    }

    #[test]
    fn reasons_are_fixed_codes() {
        use Reason::*;
        let codes = [
            SiteUnreadable,
            NotHttp,
            LoginRefused,
            NotEngineer,
            ServerGone,
            ConnectionLost,
            CpuSets,
        ];
        let codes = codes.map(Reason::code).join(" ");
        assert_eq!(
            codes,
            "site-unreadable not-http login-refused not-engineer server-gone connection-lost \
             cpu-sets"
        );
    }

    #[test]
    fn the_summary_has_its_schema_and_no_site_value() {
        let v = serde_json::to_value(Summary::default()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys.join(" "),
            "build complete decode_errors error expected_frames first_frame_ms frames gaps \
             max_gap_ms meter_frames no_source reconnects schema seconds"
        );
        // Numbers and reason codes only (P6).
        for site in ["member", "url", "pin", "host", "token"] {
            assert!(v.get(site).is_none(), "{site}");
        }
        assert_eq!(v["schema"], 1);
        assert_eq!(v["build"], BUILD);
        assert_eq!(v["complete"], false);
        assert!(v["error"].is_null());
        assert!(v["first_frame_ms"].is_null());
        // A missing field reads as its default (additive, as the guard's records).
        let empty: Summary = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Summary::default());
    }

    #[test]
    fn write_summary_replaces_the_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("soak.json");
        let read =
            |p: &Path| -> Summary { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
        let first = Summary {
            frames: 1,
            ..Summary::default()
        };
        write_summary(&out, &first).unwrap();
        assert_eq!(read(&out), first);
        let second = Summary {
            frames: 2,
            complete: true,
            error: Some(Reason::ServerGone),
            ..Summary::default()
        };
        // A reader holding the old file keeps reading it whole: the new
        // summary replaces the file, it never rewrites it in place.
        let mut held = std::fs::File::open(&out).unwrap();
        write_summary(&out, &second).unwrap();
        let mut old = String::new();
        std::io::Read::read_to_string(&mut held, &mut old).unwrap();
        assert_eq!(serde_json::from_str::<Summary>(&old).unwrap(), first);
        assert_eq!(read(&out), second);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["soak.json"], "no .tmp is left behind");
        // A file that cannot be written is an error, never a silent skip.
        let missing = dir.path().join("missing").join("soak.json");
        assert!(write_summary(&missing, &first).is_err());
    }
}
