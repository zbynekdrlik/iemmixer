//! Bundles, their records and the pins (S6 design note §5.5; P5/G8).
//!
//! A bundle is one commit's binaries and scripts, zipped by CI with a
//! `manifest.json` and a `SHA256SUMS`, and attested by digest. This module
//! holds the decisions: which names a bundle may hold, whether an unpacked
//! directory matches its sums and manifest, and which bundle may go live.
//! Unpacking the zip and walking the directory is the install code's (S6
//! plan, Task 10).

use std::io::{self, Read};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The sums file: it lists every other file and cannot sum itself.
pub const SUMS: &str = "SHA256SUMS";
pub const MANIFEST: &str = "manifest.json";
/// The one subdirectory a bundle may hold (the S1c tuning module).
pub const TUNING_DIR: &str = "tuning";

/// Files every bundle must carry (S6 design note §5.5).
pub const REQUIRED: [&str; 9] = [
    "iem-engine.exe",
    "iem-server.exe",
    "iemmixer-guard.exe",
    "iemmode.exe",
    "iem-tray.exe",
    "iem-migrate.exe",
    "hil-v1.ps1",
    "IemPc.psm1",
    MANIFEST,
];

/// `manifest.json`, written by the CI `bundle` job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub sha: String,
    pub branch: String,
    pub version: String,
    /// The CI run id.
    pub run: u64,
}

impl Manifest {
    /// Parses `manifest.json`; a UTF-8 byte-order mark (Windows PowerShell's
    /// `Set-Content -Encoding utf8`) is skipped.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        serde_json::from_str(text).map_err(|e| format!("{MANIFEST}: {e}"))
    }
}

/// The HIL result of a bundle (`hil/iem-pc`, S6 design note §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hil {
    Pending,
    Green,
    Red,
}

/// What the guard keeps per installed bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub sha: String,
    pub branch: String,
    pub run: u64,
    /// Seconds since the epoch.
    pub installed_at: u64,
    pub hil: Hil,
}

impl Record {
    /// A freshly installed bundle: HIL pending.
    pub fn installed(m: &Manifest, installed_at: u64) -> Self {
        Self {
            sha: m.sha.clone(),
            branch: m.branch.clone(),
            run: m.run,
            installed_at,
            hil: Hil::Pending,
        }
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A commit SHA as bundles are named: 40 lowercase hex digits.
pub fn valid_sha(s: &str) -> bool {
    is_lower_hex(s, 40)
}

fn plain_name(s: &str) -> bool {
    !s.is_empty() && s != "." && !s.contains("..") && !s.contains(['/', '\\', ':'])
}

/// A name `SHA256SUMS` may list: a file in the bundle's root, or one level
/// under `tuning/`; never a parent reference, a drive, a stream or `\`.
pub fn valid_name(name: &str) -> bool {
    match name.split_once('/') {
        Some((dir, rest)) => dir == TUNING_DIR && plain_name(rest),
        None => plain_name(name),
    }
}

/// The lines of `SHA256SUMS` as CI writes them: 64 lowercase hex digits, two
/// spaces, the name (`/`-separated). Empty lines are skipped. Returns
/// `(name, sha256)` pairs in file order.
pub fn parse_sums(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let n = i + 1;
        let (sum, name) = line
            .split_once("  ")
            .ok_or_else(|| format!("{SUMS} line {n}: not `<sha256>  <name>`"))?;
        if !is_lower_hex(sum, 64) {
            return Err(format!(
                "{SUMS} line {n}: {sum:?} is not a lowercase SHA-256"
            ));
        }
        if !valid_name(name) || name == SUMS {
            return Err(format!("{SUMS} line {n}: name {name:?} refused"));
        }
        if out.iter().any(|(seen, _)| seen.eq_ignore_ascii_case(name)) {
            return Err(format!("{SUMS} line {n}: {name:?} listed twice"));
        }
        out.push((name.to_owned(), sum.to_owned()));
    }
    Ok(out)
}

/// Checks an unpacked bundle directory named `dir_sha`. `files` are every
/// file in it (names relative and `/`-separated) with their SHA-256, `sums`
/// the parsed `SHA256SUMS`: every summed file is there with its digest, every
/// file there is summed (except `SHA256SUMS` itself), the required files are
/// summed, and the manifest names the directory. Returns every problem.
pub fn verify(
    files: &[(String, String)],
    sums: &[(String, String)],
    manifest: &Manifest,
    dir_sha: &str,
) -> Result<(), Vec<String>> {
    let mut bad = Vec::new();
    if !valid_sha(dir_sha) {
        bad.push(format!("directory {dir_sha:?} is not a commit SHA"));
    }
    if manifest.sha != dir_sha {
        bad.push(format!(
            "{MANIFEST} names {:?}, the directory is {dir_sha:?}",
            manifest.sha
        ));
    }
    for (name, sum) in sums {
        match files.iter().find(|(f, _)| f == name) {
            None => bad.push(format!("{name}: summed but missing")),
            Some((_, got)) if got != sum => {
                bad.push(format!("{name}: sha256 {got}, {SUMS} says {sum}"));
            }
            Some(_) => {}
        }
    }
    for (name, _) in files {
        if name != SUMS && !sums.iter().any(|(s, _)| s == name) {
            bad.push(format!("{name}: not in {SUMS}"));
        }
    }
    for req in REQUIRED {
        if !sums.iter().any(|(s, _)| s == req) {
            bad.push(format!("{req}: required but not in the bundle"));
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// `live --build` takes only a bundle from `main` whose HIL is green (G8).
pub fn may_go_live(r: &Record) -> Result<(), String> {
    if r.branch != "main" {
        return Err(format!("{} is from {:?}; live needs main", r.sha, r.branch));
    }
    if r.hil != Hil::Green {
        return Err(format!("{}: HIL {:?}; live needs green", r.sha, r.hil));
    }
    Ok(())
}

/// The live pins: `current` runs, `previous` is the revert target (and the
/// engine a prod crash loop falls back to, design §5.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pins {
    pub current: Option<String>,
    pub previous: Option<String>,
}

impl Pins {
    /// `sha` becomes current and the old current previous; promoting the
    /// current pin again changes nothing.
    pub fn promote(&mut self, sha: &str) {
        if self.current.as_deref() == Some(sha) {
            return;
        }
        self.previous = self.current.replace(sha.to_owned());
    }

    /// Back to the previous pin, which becomes current (no previous is left);
    /// returns it.
    pub fn revert(&mut self) -> Result<String, String> {
        let previous = self
            .previous
            .take()
            .ok_or_else(|| "no previous pin to revert to".to_owned())?;
        self.current = Some(previous.clone());
        Ok(previous)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 as lowercase hex (the form of `SHA256SUMS`).
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// SHA-256 of everything `r` yields, as lowercase hex (files of any size).
pub fn sha256_read(mut r: impl Read) -> io::Result<String> {
    let mut h = Sha256::new();
    io::copy(&mut r, &mut h)?;
    Ok(hex(&h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn manifest() -> Manifest {
        Manifest {
            sha: SHA.into(),
            branch: "dev".into(),
            version: "2.0.0-dev.9".into(),
            run: 42,
        }
    }

    /// A complete bundle: (files in the directory, parsed sums).
    fn bundle() -> (Vec<(String, String)>, Vec<(String, String)>) {
        let mut names: Vec<&str> = REQUIRED.to_vec();
        names.extend(["LICENSE-iem-engine", "tuning/IemTuning.psm1"]);
        let sums: Vec<(String, String)> = names
            .iter()
            .map(|n| ((*n).to_owned(), sha256_hex(n.as_bytes())))
            .collect();
        let mut files = sums.clone();
        files.push((SUMS.to_owned(), sha256_hex(b"the sums")));
        (files, sums)
    }

    fn sums_text(sums: &[(String, String)]) -> String {
        sums.iter().map(|(n, s)| format!("{s}  {n}\n")).collect()
    }

    #[test]
    fn digests_are_lowercase_sha256() {
        assert_eq!(sha256_hex(b"abc"), ABC);
        assert_eq!(sha256_hex(b""), EMPTY);
        assert_eq!(sha256_read(&b"abc"[..]).unwrap(), ABC);
        assert_eq!(sha256_read(&b""[..]).unwrap(), EMPTY);
        let big = vec![7u8; 100_000];
        assert_eq!(sha256_read(big.as_slice()).unwrap(), sha256_hex(&big));
    }

    #[test]
    fn a_sha_is_40_lowercase_hex_digits() {
        assert!(valid_sha(SHA));
        assert!(valid_sha(&"f".repeat(40)));
        assert!(!valid_sha(&SHA[..39]));
        assert!(!valid_sha(&format!("{SHA}0")));
        assert!(!valid_sha(&SHA.to_uppercase()));
        assert!(!valid_sha(&format!("{}g", &SHA[..39])));
        assert!(!valid_sha("abc"));
        assert!(!valid_sha(""));
    }

    #[test]
    fn names_stay_inside_the_bundle() {
        for ok in [
            "iem-engine.exe",
            "manifest.json",
            "LICENSE-iem-engine",
            "tuning/IemTuning.psm1",
            "a b.txt",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a..b",
            "../x",
            "/abs",
            "bin/x.exe",
            "tuning/",
            "tuning/..",
            "tuning/a/b",
            "tuning\\a",
            "a\\b",
            "C:x",
            "x.exe:stream",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn sums_parse_in_file_order() {
        let text = format!("{ABC}  iem-engine.exe\n\n{EMPTY}  tuning/IemTuning.psm1\r\n");
        assert_eq!(
            parse_sums(&text).unwrap(),
            [
                ("iem-engine.exe".to_owned(), ABC.to_owned()),
                ("tuning/IemTuning.psm1".to_owned(), EMPTY.to_owned()),
            ]
        );
        assert!(parse_sums("").unwrap().is_empty());
        assert!(parse_sums("\n\r\n").unwrap().is_empty());
    }

    #[test]
    fn bad_sum_lines_are_refused_with_their_line() {
        let cases = [
            (
                format!("{ABC} iem-engine.exe"),
                "SHA256SUMS line 1: not `<sha256>  <name>`".to_owned(),
            ),
            (
                format!("{}  a.exe", ABC.to_uppercase()),
                format!(
                    "SHA256SUMS line 1: {:?} is not a lowercase SHA-256",
                    ABC.to_uppercase()
                ),
            ),
            (
                format!("{}  a.exe", &ABC[..63]),
                format!(
                    "SHA256SUMS line 1: {:?} is not a lowercase SHA-256",
                    &ABC[..63]
                ),
            ),
            (
                format!("{ABC}  a.exe\n{ABC}  ../evil.exe"),
                "SHA256SUMS line 2: name \"../evil.exe\" refused".to_owned(),
            ),
            (
                format!("{ABC}  a.exe\n\n{ABC}  SHA256SUMS"),
                "SHA256SUMS line 3: name \"SHA256SUMS\" refused".to_owned(),
            ),
            (
                format!("{ABC}  a.exe\n{EMPTY}  A.EXE"),
                "SHA256SUMS line 2: \"A.EXE\" listed twice".to_owned(),
            ),
        ];
        for (text, want) in cases {
            assert_eq!(parse_sums(&text), Err(want), "{text:?}");
        }
    }

    #[test]
    fn a_complete_bundle_verifies() {
        let (files, sums) = bundle();
        assert_eq!(parse_sums(&sums_text(&sums)).unwrap(), sums);
        assert_eq!(verify(&files, &sums, &manifest(), SHA), Ok(()));
        // SHA256SUMS present but unsummed is accepted; so is its absence.
        let without_sums: Vec<_> = files.iter().filter(|(n, _)| n != SUMS).cloned().collect();
        assert_eq!(verify(&without_sums, &sums, &manifest(), SHA), Ok(()));
    }

    #[test]
    fn a_tampered_file_is_refused() {
        let (mut files, sums) = bundle();
        let engine = files
            .iter_mut()
            .find(|(n, _)| n == "iem-engine.exe")
            .unwrap();
        engine.1 = ABC.to_owned();
        let want = sha256_hex(b"iem-engine.exe");
        assert_eq!(
            verify(&files, &sums, &manifest(), SHA),
            Err(vec![format!(
                "iem-engine.exe: sha256 {ABC}, SHA256SUMS says {want}"
            )])
        );
    }

    #[test]
    fn a_summed_file_that_is_missing_is_refused() {
        let (mut files, sums) = bundle();
        files.retain(|(n, _)| n != "tuning/IemTuning.psm1");
        assert_eq!(
            verify(&files, &sums, &manifest(), SHA),
            Err(vec!["tuning/IemTuning.psm1: summed but missing".to_owned()])
        );
    }

    #[test]
    fn an_unsummed_file_is_refused() {
        let (mut files, sums) = bundle();
        files.push(("extra.dll".to_owned(), ABC.to_owned()));
        assert_eq!(
            verify(&files, &sums, &manifest(), SHA),
            Err(vec!["extra.dll: not in SHA256SUMS".to_owned()])
        );
    }

    #[test]
    fn a_missing_required_file_is_refused() {
        let (mut files, mut sums) = bundle();
        files.retain(|(n, _)| n != "hil-v1.ps1");
        sums.retain(|(n, _)| n != "hil-v1.ps1");
        assert_eq!(
            verify(&files, &sums, &manifest(), SHA),
            Err(vec![
                "hil-v1.ps1: required but not in the bundle".to_owned()
            ])
        );
        // Every required file counts.
        for req in REQUIRED {
            let (mut files, mut sums) = bundle();
            files.retain(|(n, _)| n != req);
            sums.retain(|(n, _)| n != req);
            assert!(verify(&files, &sums, &manifest(), SHA).is_err(), "{req}");
        }
    }

    #[test]
    fn the_manifest_must_name_the_directory() {
        let (files, sums) = bundle();
        let other = "f".repeat(40);
        assert_eq!(
            verify(&files, &sums, &manifest(), &other),
            Err(vec![format!(
                "manifest.json names {SHA:?}, the directory is {other:?}"
            )])
        );
        let short = &SHA[..12];
        let m = Manifest {
            sha: short.to_owned(),
            ..manifest()
        };
        assert_eq!(
            verify(&files, &sums, &m, short),
            Err(vec![format!("directory {short:?} is not a commit SHA")])
        );
    }

    #[test]
    fn every_problem_is_reported() {
        let (mut files, mut sums) = bundle();
        files.push(("extra.dll".to_owned(), ABC.to_owned()));
        sums.retain(|(n, _)| n != "iemmode.exe");
        files.retain(|(n, _)| n != "iemmode.exe");
        let problems = verify(&files, &sums, &manifest(), &"a".repeat(40)).unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn the_manifest_parses_with_or_without_a_bom() {
        let json = format!(r#"{{"sha":"{SHA}","branch":"dev","version":"2.0.0-dev.9","run":42}}"#);
        assert_eq!(Manifest::parse(&json).unwrap(), manifest());
        assert_eq!(
            Manifest::parse(&format!("\u{feff}{json}")).unwrap(),
            manifest()
        );
        let err = Manifest::parse("{}").unwrap_err();
        assert!(err.starts_with("manifest.json: "), "{err}");
    }

    #[test]
    fn an_installed_record_waits_for_hil() {
        assert_eq!(
            Record::installed(&manifest(), 1_790_000_000),
            Record {
                sha: SHA.into(),
                branch: "dev".into(),
                run: 42,
                installed_at: 1_790_000_000,
                hil: Hil::Pending,
            }
        );
    }

    #[test]
    fn only_a_green_main_bundle_goes_live() {
        let r = |branch: &str, hil| Record {
            hil,
            branch: branch.into(),
            ..Record::installed(&manifest(), 0)
        };
        assert_eq!(may_go_live(&r("main", Hil::Green)), Ok(()));
        assert_eq!(
            may_go_live(&r("dev", Hil::Green)),
            Err(format!("{SHA} is from \"dev\"; live needs main"))
        );
        assert_eq!(
            may_go_live(&r("main", Hil::Pending)),
            Err(format!("{SHA}: HIL Pending; live needs green"))
        );
        assert_eq!(
            may_go_live(&r("main", Hil::Red)),
            Err(format!("{SHA}: HIL Red; live needs green"))
        );
    }

    #[test]
    fn pins_promote_and_revert() {
        let mut p = Pins::default();
        assert_eq!(p.revert(), Err("no previous pin to revert to".to_owned()));
        p.promote("a");
        assert_eq!(
            p,
            Pins {
                current: Some("a".into()),
                previous: None
            }
        );
        p.promote("b");
        assert_eq!(
            p,
            Pins {
                current: Some("b".into()),
                previous: Some("a".into())
            }
        );
        // Promoting the current pin again keeps the revert target.
        p.promote("b");
        assert_eq!(
            p,
            Pins {
                current: Some("b".into()),
                previous: Some("a".into())
            }
        );
        assert_eq!(p.revert(), Ok("a".to_owned()));
        assert_eq!(
            p,
            Pins {
                current: Some("a".into()),
                previous: None
            }
        );
        assert_eq!(p.revert(), Err("no previous pin to revert to".to_owned()));
        assert_eq!(p.current.as_deref(), Some("a"));
    }

    #[test]
    fn records_and_pins_round_trip_as_json() {
        let r = Record {
            hil: Hil::Green,
            ..Record::installed(&manifest(), 5)
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains(r#""hil":"green""#), "{text}");
        assert_eq!(serde_json::from_str::<Record>(&text).unwrap(), r);
        let p: Pins = serde_json::from_str("{}").unwrap();
        assert_eq!(p, Pins::default());
    }
}
