//! The soak harness (S7 design note §4): signs in as the engineer, opens
//! one mixer socket and one listen socket on one member's mix at the LAN
//! address the server names (`/api/site`), decodes the Opus frames and
//! counts frames, gaps and meter frames into a JSON summary. It reads only:
//! it sends no mixer command. It never opens a socket twice (#10): a lost
//! socket ends the run. The engineer's credential is exactly one of two
//! (#10 decision, 2026-10-07): on the server's PC its JWT secret file
//! (`--jwt-secret-file`), with which the client signs its own token and
//! never logs in; in CI the PIN from `IEM_SOAK_PIN` (never from the command
//! line) for the server's login. Nothing here ends a process: each socket
//! ends with a WebSocket Close.
//!
//! This file is the pure core: the arguments, the credential and the
//! engineer token, the URLs, the gap clock, the event classes and the
//! summary. [`tally`] counts a run into its summary (pure too); [`net`] is
//! the wire: the sign-in and the socket threads.

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

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jsonwebtoken::{EncodingKey, Header};
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
/// The engineer's id: the login's member and the token's subject (the
/// server's `ENGINEER_ID`).
pub const ENGINEER: &str = "engineer";
/// A token the client signs outlives `--seconds` by this much (10 min): the
/// build check, the opens and the closes come on top of the run. The server
/// reads the token only when a socket opens.
pub const TOKEN_MARGIN: u64 = 600;
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
/// `--expect-build`: a commit's hex digits.
const COMMIT_LEN: usize = 40;
/// A server names its build by this many of the commit's first hex digits
/// at least: git's short hash, as HIL v1's `Test-IemHilVersion` takes it.
pub const MIN_HASH: usize = 7;

pub const USAGE: &str = "\
iem-soakclient --member ID --seconds N --out FILE --expect-build SHA
               [--jwt-secret-file PATH] [--base URL] [--direct]
               [--cpu-sets IDS]

Goes to the LAN address the server at --base names (/api/site; with --direct
to --base itself), checks that /api/version there names build SHA, signs in
as the engineer, opens one mixer socket and one listen socket on the mix of
member ID, and writes a JSON summary to FILE every minute and at the end.
It reads only, and opens no socket twice: a lost socket ends the run.

The engineer's credential is exactly one of two: on the server's PC
--jwt-secret-file, with which it signs its own token (no login); else the
PIN from IEM_SOAK_PIN (never an argument) for the server's login.

  --expect-build SHA      the server's commit: 40 lower-case hex digits
  --seconds N             1 to 36000
  --jwt-secret-file PATH  the server's jwt_secret file
  --base URL              http://HOST[:PORT], default http://127.0.0.1
  --cpu-sets IDS          CPU Set ids, e.g. 256,257 (Windows only)";

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
    /// `--expect-build`: the commit the server must name in `/api/version`
    /// before anything else is sent to it ([`names_build`]).
    pub expect_build: String,
    /// `--jwt-secret-file`: the server's JWT secret file, on the server's
    /// PC; [`credential`] takes it or the PIN, never both.
    pub jwt_secret_file: Option<PathBuf>,
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
    let mut expect_build = None;
    let mut jwt_secret_file = None;
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
            "--expect-build" => &mut expect_build,
            "--jwt-secret-file" => &mut jwt_secret_file,
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
    let expect_build = expect_build.ok_or("--expect-build is required")?;
    if !is_commit(&expect_build) {
        return Err("--expect-build must be a commit's 40 lower-case hex digits".to_owned());
    }
    if jwt_secret_file.as_deref() == Some("") {
        return Err("--jwt-secret-file needs a path".to_owned());
    }
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
        expect_build,
        jwt_secret_file: jwt_secret_file.map(PathBuf::from),
        cpu_sets,
    })
}

/// A commit as `--expect-build` takes it: 40 lower-case hex digits.
fn is_commit(s: &str) -> bool {
    s.len() == COMMIT_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `/api/version`'s answer names `build` (a commit, as `--expect-build`): its
/// `git_hash`, the server's commit as `git rev-parse --short` printed it at
/// build time (`iem_core::git_hash`), is at least [`MIN_HASH`] characters
/// and a prefix of `build`. The predecessor app's answer names its own
/// commit, never this one; any other answer names none.
///
/// The same rule has two other copies: HIL v1's `Test-IemHilVersion`
/// (`scripts/iem-pc/IemPc.psm1`, lower case and an ordinal prefix, as here)
/// and the guard's `iem_guard::effects::web::version_matches` (any case).
/// This crate links no server or guard crate, so the rule is repeated; a
/// change to one is made to all three.
pub fn names_build(answer: &str, build: &str) -> bool {
    let answer: serde_json::Value = serde_json::from_str(answer).unwrap_or_default();
    answer
        .get("git_hash")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|hash| hash.len() >= MIN_HASH && build.starts_with(hash))
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

/// How the client signs in as the engineer (#10 decision, 2026-10-07).
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// `--jwt-secret-file`, on the server's PC: read when the run starts
    /// ([`read_secret`]), before any request; the client signs its own
    /// token with it ([`engineer_token`]) and never logs in.
    SecretFile(PathBuf),
    /// `IEM_SOAK_PIN` ([`pin_from`]), in CI: the server's login.
    Pin(String),
}

/// Neither the PIN nor the path (P6).
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Credential::SecretFile(_) => "SecretFile(..)",
            Credential::Pin(_) => "Pin(..)",
        })
    }
}

/// Exactly one credential: `secret_file` (`--jwt-secret-file`) or `pin`
/// (`IEM_SOAK_PIN` as read, set at all: an empty value counts), judged by
/// [`pin_from`]. Both or neither is a usage error that names the two, never
/// a value.
pub fn credential(secret_file: Option<&Path>, pin: Option<String>) -> Result<Credential, String> {
    match (secret_file, pin) {
        (Some(_), Some(_)) => Err(format!(
            "--jwt-secret-file and {PIN_ENV} are both given: give one"
        )),
        (Some(path), None) => Ok(Credential::SecretFile(path.to_owned())),
        (None, Some(pin)) => pin_from(Some(pin)).map(Credential::Pin),
        (None, None) => Err(format!(
            "give --jwt-secret-file (on the server's PC) or {PIN_ENV}"
        )),
    }
}

/// The server's JWT signing key as the server holds it: its `jwt_secret`
/// file's text, trimmed (`iem_server::secrets`), whose bytes are the HS256
/// key (`auth::issue_token`). Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The key in `text` as the server reads its file: trimmed; none when
    /// nothing is left.
    pub fn from_text(text: &str) -> Option<Self> {
        let key = text.trim();
        (!key.is_empty()).then(|| Self(key.to_owned()))
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The secret in the server's `jwt_secret` file at `path`, read as the
/// server reads it ([`Secret::from_text`] of its UTF-8 text). A file that
/// is missing, unreadable, not UTF-8 or blank is `secret-unreadable`, as it
/// stops the server's start. The file is never created here (the server's
/// first start makes it), and neither its path nor its text reaches an
/// error.
pub fn read_secret(path: &Path) -> Result<Secret, Reason> {
    let text = fs::read_to_string(path).map_err(|_| Reason::SecretUnreadable)?;
    Secret::from_text(&text).ok_or(Reason::SecretUnreadable)
}

/// The engineer token's claims: the server's `AuthClaims` (`iem_core`; the
/// tests read the token back into it).
#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    engineer: bool,
    exp: u64,
    iat: u64,
}

/// The engineer's token as the server's login issues it
/// (`iem_server::auth::issue_token`: the default header, HS256, the
/// secret's bytes), issued at the Unix second `now` and valid for `seconds`
/// and [`TOKEN_MARGIN`] more. A token that cannot be signed with the secret
/// is `secret-unreadable`.
pub fn engineer_token(secret: &Secret, now: u64, seconds: u64) -> Result<String, Reason> {
    let claims = Claims {
        sub: ENGINEER,
        engineer: true,
        exp: now + seconds + TOKEN_MARGIN,
        iat: now,
    };
    let key = EncodingKey::from_secret(secret.0.as_bytes());
    jsonwebtoken::encode(&Header::default(), &claims, &key).map_err(|_| Reason::SecretUnreadable)
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

/// Where the build check, the login and the sockets go: the server's own
/// LAN address (`/api/site`'s `lan_url`), or `--base` itself with
/// `--direct`.
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
    /// The server there answered `/api/version` without naming
    /// `--expect-build` (another build, the predecessor app, no version
    /// route): nothing else was sent to it.
    WrongServer,
    /// The login was refused (the PIN, or the login protection).
    LoginRefused,
    /// The login was not the engineer's (the listen socket is engineer-only).
    NotEngineer,
    /// The build check or the login got no answer, or a socket could not
    /// be opened (there is no second try).
    ServerGone,
    /// A socket closed, failed or stayed silent for the idle bound before
    /// the end: no socket is opened twice (#10).
    ConnectionLost,
    /// The process could not be placed on the given CPU Sets.
    CpuSets,
    /// `--jwt-secret-file` could not be read, is not UTF-8 text or is
    /// blank (or no token could be signed with it): nothing was sent.
    SecretUnreadable,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::SiteUnreadable => "site-unreadable",
            Reason::NotHttp => "not-http",
            Reason::WrongServer => "wrong-server",
            Reason::LoginRefused => "login-refused",
            Reason::NotEngineer => "not-engineer",
            Reason::ServerGone => "server-gone",
            Reason::ConnectionLost => "connection-lost",
            Reason::CpuSets => "cpu-sets",
            Reason::SecretUnreadable => "secret-unreadable",
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
mod tests;
