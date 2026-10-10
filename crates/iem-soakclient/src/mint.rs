//! `iem-soakclient token` (S7 plan Task 23; the #10 decision of 2026-10-08:
//! no band PIN in GitHub for the live run either). On the server's PC the
//! ops `live.yml` job `pc-begin` mints the run's engineer token and one
//! member token with the server's own JWT secret (`--jwt-secret-file`, read
//! as [`read_secret`] reads it for the soak), valid for the run's length,
//! and hands them to the browser job. Nothing logs in.
//!
//! The token goes only to the `--out` file: a new file (an existing one is
//! never written through or replaced), owner-only where the platform allows
//! it. Stdout gets the fixed word [`WRITTEN`]; stderr a usage message
//! naming flags, or a fixed code ([`MintError::code`]). No token, secret,
//! path or member id is ever printed (P6).
//!
//! The server reads these tokens as it reads its login's
//! (`iem_server::auth::extract_claims`: HS256, the secret's bytes, the
//! default validation), so [`token`] is the one signing path:
//! [`crate::engineer_token`], the soak's own, calls it too.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{EncodingKey, Header};
use serde::Serialize;

use crate::{ENGINEER, Reason, Secret, read_secret, valid_member};

/// `--seconds` at least: a minute.
pub const MIN_TOKEN_SECONDS: u64 = 60;
/// `--seconds` at most: 2 h, twice the live run's 3600 s.
pub const MAX_TOKEN_SECONDS: u64 = 7_200;
/// The one line on stdout once the token is in its file.
pub const WRITTEN: &str = "token-written";

pub const USAGE: &str = "\
iem-soakclient token --jwt-secret-file PATH --sub ID [--engineer]
                     --seconds N --out FILE

Signs a token for ID with the server's JWT secret, valid for N seconds from
now, and writes it to FILE alone: a new file, owner-only where the platform
allows it. It prints only token-written; the token never reaches stdout or
stderr. Nothing is sent anywhere.

  --jwt-secret-file PATH  the server's jwt_secret file
  --sub ID                engineer (with --engineer) or a member's id
  --engineer              the engineer's token: --sub engineer, and only it
  --seconds N             60 to 7200
  --out FILE              must not exist yet";

/// `iem-soakclient token`'s command line.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenArgs {
    /// `--jwt-secret-file`: the server's `jwt_secret`.
    pub jwt_secret_file: PathBuf,
    /// `--sub`: the token's subject, `engineer` or a member's id.
    pub sub: String,
    /// `--engineer`: the token's `engineer` claim; set exactly when `sub` is
    /// `engineer`.
    pub engineer: bool,
    /// `--seconds`, [`MIN_TOKEN_SECONDS`] to [`MAX_TOKEN_SECONDS`].
    pub seconds: u64,
    /// `--out`: the new file that gets the token.
    pub out: PathBuf,
}

/// Neither the member nor a path (P6).
impl fmt::Debug for TokenArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenArgs")
            .field("engineer", &self.engineer)
            .field("seconds", &self.seconds)
            .finish_non_exhaustive()
    }
}

/// Reads the arguments after `token`. `Err` is the usage error; it names
/// flags, never a value.
pub fn parse_args(args: &[String]) -> Result<TokenArgs, String> {
    let mut jwt_secret_file = None;
    let mut sub = None;
    let mut engineer = false;
    let mut seconds = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let slot: &mut Option<String> = match flag.as_str() {
            "--engineer" => {
                if engineer {
                    return Err("--engineer is given twice".to_owned());
                }
                engineer = true;
                continue;
            }
            "--jwt-secret-file" => &mut jwt_secret_file,
            "--sub" => &mut sub,
            "--seconds" => &mut seconds,
            "--out" => &mut out,
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
    let jwt_secret_file = jwt_secret_file.ok_or("--jwt-secret-file is required")?;
    if jwt_secret_file.is_empty() {
        return Err("--jwt-secret-file needs a path".to_owned());
    }
    let sub = sub.ok_or("--sub is required")?;
    if !valid_member(&sub) {
        return Err("--sub must be 1 to 64 letters, digits, '_' or '-'".to_owned());
    }
    match (sub == ENGINEER, engineer) {
        (true, false) => return Err(format!("--sub {ENGINEER} needs --engineer")),
        (false, true) => return Err(format!("--engineer is for --sub {ENGINEER} only")),
        _ => {}
    }
    let seconds = seconds
        .ok_or("--seconds is required")?
        .parse::<u64>()
        .ok()
        .filter(|s| (MIN_TOKEN_SECONDS..=MAX_TOKEN_SECONDS).contains(s))
        .ok_or_else(|| {
            format!(
                "--seconds must be a whole number from {MIN_TOKEN_SECONDS} to {MAX_TOKEN_SECONDS}"
            )
        })?;
    let out = out.ok_or("--out is required")?;
    if out.is_empty() {
        return Err("--out needs a path".to_owned());
    }
    Ok(TokenArgs {
        jwt_secret_file: PathBuf::from(jwt_secret_file),
        sub,
        engineer,
        seconds,
        out: PathBuf::from(out),
    })
}

/// A token's claims: the server's `AuthClaims` (`iem_core`; the tests read
/// the tokens back into it).
#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    engineer: bool,
    exp: u64,
    iat: u64,
}

/// The token for `sub` as the server's login issues one
/// (iem-server's `auth::issue_token`, private: the default header, HS256, the
/// secret's bytes), issued at the Unix second `now` and valid for `seconds`.
/// A token that cannot be signed with the secret is `secret-unreadable`
/// (not seen with HS256).
pub fn token(
    secret: &Secret,
    sub: &str,
    engineer: bool,
    now: u64,
    seconds: u64,
) -> Result<String, Reason> {
    let claims = Claims {
        sub,
        engineer,
        exp: now + seconds,
        iat: now,
    };
    let key = EncodingKey::from_secret(secret.0.as_bytes());
    jsonwebtoken::encode(&Header::default(), &claims, &key).map_err(|_| Reason::SecretUnreadable)
}

/// Why `iem-soakclient token` wrote no token: a fixed code, never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintError {
    /// `--jwt-secret-file` is missing, unreadable, not UTF-8 or blank (as
    /// for the soak, [`read_secret`]), or the token could not be signed.
    SecretUnreadable,
    /// `--out` exists already, or could not be created or written.
    TokenUnwritable,
    /// The system clock reads before 1970: a token from it would be over
    /// before it is used, so none is written.
    ClockUnreadable,
}

impl MintError {
    pub fn code(self) -> &'static str {
        match self {
            MintError::SecretUnreadable => "secret-unreadable",
            MintError::TokenUnwritable => "token-unwritable",
            MintError::ClockUnreadable => "clock-unreadable",
        }
    }
}

/// The Unix second of `t`, the token's `iat`, as the server's clock reads
/// it (`mixer_ws::claims_of`; the server runs on the same PC). A clock
/// before 1970 is `clock-unreadable`, never a token over since 1970.
pub fn unix_seconds(t: SystemTime) -> Result<u64, MintError> {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| MintError::ClockUnreadable)
}

/// Reads the secret, signs the token at the Unix second `now` and writes
/// it to `args.out` ([`write_token`]). The secret is read first: an
/// unreadable one creates no file.
pub fn run(args: &TokenArgs, now: u64) -> Result<(), MintError> {
    let secret = read_secret(&args.jwt_secret_file).map_err(|_| MintError::SecretUnreadable)?;
    let token = token(&secret, &args.sub, args.engineer, now, args.seconds)
        .map_err(|_| MintError::SecretUnreadable)?;
    write_token(&args.out, &token).map_err(|_| MintError::TokenUnwritable)
}

/// Writes `token` alone to `path`, a NEW file: one that exists (a symlink
/// too) is never written through or replaced. On Unix it is created
/// owner-only (0600); on Windows it inherits its folder's ACL, so the ops
/// job makes that folder its runner user's only. A file that could not be
/// written whole is removed, so a part of a token never stays behind.
pub fn write_token(path: &Path, token: &str) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let written = file
        .write_all(token.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        // The write's own error is the one reported; a part left behind
        // holds no whole token and is refused as existing by the next run.
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tests::{SECRET, as_server, unix_now};
    use crate::{TOKEN_MARGIN, engineer_token};

    fn parse(list: &[&str]) -> Result<TokenArgs, String> {
        let list: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
        parse_args(&list)
    }

    /// A member's arguments, with `extra` appended.
    fn member(extra: &[&str]) -> Result<TokenArgs, String> {
        let mut list = vec![
            "--jwt-secret-file",
            "secrets/jwt_secret",
            "--sub",
            "member9",
            "--seconds",
            "3600",
            "--out",
            "member.token",
        ];
        list.extend_from_slice(extra);
        parse(&list)
    }

    /// The engineer's arguments, with `extra` appended.
    fn engineer(extra: &[&str]) -> Result<TokenArgs, String> {
        let mut list = vec![
            "--jwt-secret-file",
            "secrets/jwt_secret",
            "--sub",
            "engineer",
            "--seconds",
            "3600",
            "--out",
            "engineer.token",
        ];
        list.extend_from_slice(extra);
        parse(&list)
    }

    fn secret() -> Secret {
        Secret::from_text(SECRET).unwrap()
    }

    #[test]
    fn a_member_token_reads_back_as_that_member_and_not_engineer() {
        let now = unix_now();
        let token = token(&secret(), "member9", false, now, 3_600).unwrap();
        let claims = as_server(&token, SECRET).expect("the server reads it");
        assert_eq!(claims.sub, "member9");
        assert!(!claims.engineer);
        assert_eq!(claims.iat, now);
        assert_eq!(claims.exp, now + 3_600);
        // HS256, the server's `Header::default()`; another secret is
        // refused, and so is a token whose time is over.
        let header = jsonwebtoken::decode_header(&token).unwrap();
        assert_eq!(header.alg, jsonwebtoken::Algorithm::HS256);
        assert!(as_server(&token, "another-synthetic-secret").is_none());
        let old = token_at(now - 7_200, 60);
        assert!(as_server(&old, SECRET).is_none());
        // The browser puts it into a socket's query as it is.
        let url_safe = |b: u8| b.is_ascii_alphanumeric() || b"-_.".contains(&b);
        assert!(token.bytes().all(url_safe), "{token}");
    }

    fn token_at(now: u64, seconds: u64) -> String {
        token(&secret(), "member9", false, now, seconds).unwrap()
    }

    #[test]
    fn an_engineer_token_reads_back_as_the_engineer() {
        let now = unix_now();
        let token = token(&secret(), ENGINEER, true, now, 3_600).unwrap();
        let claims = as_server(&token, SECRET).expect("the server reads it");
        assert_eq!(claims.sub, "engineer");
        assert!(claims.engineer);
        assert_eq!(claims.iat, now);
        assert_eq!(claims.exp, now + 3_600);
        // The soak's own engineer token is this one, with the margin on
        // top of the run's seconds: one signing path.
        assert_eq!(
            engineer_token(&secret(), now, 60),
            mint_engineer(now, 60 + TOKEN_MARGIN)
        );
    }

    fn mint_engineer(now: u64, seconds: u64) -> Result<String, crate::Reason> {
        token(&secret(), ENGINEER, true, now, seconds)
    }

    #[test]
    fn the_arguments_and_their_flags() {
        assert_eq!(
            member(&[]),
            Ok(TokenArgs {
                jwt_secret_file: PathBuf::from("secrets/jwt_secret"),
                sub: "member9".to_owned(),
                engineer: false,
                seconds: 3_600,
                out: PathBuf::from("member.token"),
            })
        );
        let engineer = engineer(&["--engineer"]).unwrap();
        assert_eq!(
            (engineer.sub.as_str(), engineer.engineer),
            ("engineer", true)
        );
        // The flag's place does not matter.
        let first = parse(&[
            "--engineer",
            "--sub",
            "engineer",
            "--jwt-secret-file",
            "s",
            "--seconds",
            "60",
            "--out",
            "o",
        ]);
        assert!(first.unwrap().engineer);
    }

    #[test]
    fn the_arguments_refuse_a_member_with_engineer_and_engineer_without_it() {
        let with = member(&["--engineer"]).unwrap_err();
        let without = engineer(&[]).unwrap_err();
        assert!(with.contains("--engineer"), "{with}");
        assert!(without.contains("--engineer"), "{without}");
        assert_ne!(with, without);
        // The member's id is never repeated.
        assert!(!with.contains("member9"), "{with}");
        // Each alone is fine.
        assert!(!member(&[]).unwrap().engineer);
        assert!(engineer(&["--engineer"]).unwrap().engineer);
    }

    #[test]
    fn seconds_are_60_to_7200() {
        let seconds = |n: &str| {
            parse(&[
                "--jwt-secret-file",
                "s",
                "--sub",
                "member9",
                "--seconds",
                n,
                "--out",
                "o",
            ])
            .map(|a| a.seconds)
        };
        assert_eq!((MIN_TOKEN_SECONDS, MAX_TOKEN_SECONDS), (60, 7_200));
        assert_eq!(seconds("60"), Ok(60));
        assert_eq!(seconds("7200"), Ok(7_200));
        for bad in ["59", "7201", "0", "", "-60", "60.5", "1e3", "abc"] {
            let e = seconds(bad).unwrap_err();
            assert!(
                e.contains("--seconds") && e.contains("60 to 7200"),
                "{bad:?}: {e}"
            );
        }
    }

    #[test]
    fn a_bad_sub_is_a_usage_error_naming_no_value() {
        let long = "z".repeat(65);
        let bad = [
            "zyxq wvq",
            "zyxq/wvq",
            "zyxqwvq.",
            "zyxq:wvq",
            "\u{10f}qxwzy",
            long.as_str(),
        ];
        for sub in bad {
            let e = parse(&[
                "--jwt-secret-file",
                "s",
                "--sub",
                sub,
                "--seconds",
                "60",
                "--out",
                "o",
            ])
            .unwrap_err();
            assert!(e.contains("--sub"), "{e}");
            assert!(!e.contains(sub), "{e}");
        }
        // 64 characters is still an id.
        let longest = "z".repeat(64);
        let ok = parse(&[
            "--jwt-secret-file",
            "s",
            "--sub",
            &longest,
            "--seconds",
            "60",
            "--out",
            "o",
        ]);
        assert_eq!(ok.map(|a| a.sub), Ok(longest));
    }

    #[test]
    fn every_other_bad_argument_is_a_usage_error_naming_no_value() {
        let all = [
            "--jwt-secret-file",
            "zyxsecretpath",
            "--sub",
            "member9",
            "--seconds",
            "60",
            "--out",
            "zyxoutpath",
        ];
        // Each value flag is required and needs a value.
        for i in (0..all.len()).step_by(2) {
            let flag = all[i];
            let mut without = all.to_vec();
            without.remove(i);
            without.remove(i);
            let e = parse(&without).unwrap_err();
            assert!(e.contains(flag), "{flag}: {e}");
            // Its value gone: the next flag (or the end) follows it.
            let mut bare = all.to_vec();
            bare.remove(i + 1);
            let e = parse(&bare).unwrap_err();
            assert!(e.contains(flag) && e.contains("value"), "{flag}: {e}");
        }
        for (flag, value) in [("--jwt-secret-file", ""), ("--out", "")] {
            let mut empty = all.to_vec();
            let at = empty.iter().position(|a| *a == flag).unwrap();
            empty[at + 1] = value;
            assert!(parse(&empty).unwrap_err().contains(flag), "{flag}");
        }
        // A flag's value never starts with `--`.
        let mut swallowed = all.to_vec();
        swallowed[1] = "--sub";
        assert!(parse(&swallowed).is_err());
        // Twice, unknown, a PIN: refused, and no value is repeated.
        let mut errors = Vec::new();
        for extra in [
            &["--sub", "member8"][..],
            &["--engineer", "--engineer"][..],
            &["--zyxflag"][..],
            &["--pin", "1234"][..],
            &["--member", "member9"][..],
        ] {
            let mut list = all.to_vec();
            list.extend_from_slice(extra);
            errors.push(parse(&list).unwrap_err());
        }
        for e in &errors {
            for value in ["zyxsecretpath", "zyxoutpath", "member8", "zyxflag", "1234"] {
                assert!(!e.contains(value), "{e}");
            }
        }
        assert!(errors[0].contains("--sub") && errors[0].contains("twice"));
        assert!(errors[1].contains("--engineer") && errors[1].contains("twice"));
    }

    #[test]
    fn the_arguments_print_no_member_and_no_path() {
        let args = member(&[]).unwrap();
        assert_eq!(
            format!("{args:?}"),
            "TokenArgs { engineer: false, seconds: 3600, .. }"
        );
    }

    #[test]
    fn the_token_goes_only_to_the_out_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret_file = dir.path().join("jwt_secret");
        std::fs::write(&secret_file, format!("{SECRET}\n")).unwrap();
        let args = |sub: &str, engineer: bool, out: &str| TokenArgs {
            jwt_secret_file: secret_file.clone(),
            sub: sub.to_owned(),
            engineer,
            seconds: 3_600,
            out: dir.path().join(out),
        };
        let now = unix_now();
        // The file holds the token alone, the same one `token` signs.
        let member = args("member9", false, "member.token");
        assert_eq!(run(&member, now), Ok(()));
        let written = std::fs::read_to_string(&member.out).unwrap();
        assert_eq!(
            Ok(written.clone()),
            token(&secret(), "member9", false, now, 3_600)
        );
        let claims = as_server(&written, SECRET).unwrap();
        assert_eq!((claims.sub.as_str(), claims.engineer), ("member9", false));
        let engineer = args(ENGINEER, true, "engineer.token");
        assert_eq!(run(&engineer, now), Ok(()));
        let claims = as_server(&std::fs::read_to_string(&engineer.out).unwrap(), SECRET);
        assert_eq!(claims.map(|c| c.engineer), Some(true));
        // Owner-only where the platform has modes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&member.out).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // An existing file is never written through or replaced.
        assert_eq!(run(&member, now + 1), Err(MintError::TokenUnwritable));
        assert_eq!(std::fs::read_to_string(&member.out).unwrap(), written);
        // No directory for it: unwritable, nothing made.
        let nowhere = args("member9", false, "missing/member.token");
        assert_eq!(run(&nowhere, now), Err(MintError::TokenUnwritable));
        assert!(!dir.path().join("missing").exists());
        // An unreadable secret: no file at all.
        let unread = TokenArgs {
            jwt_secret_file: dir.path().join("no_secret"),
            ..args("member9", false, "unread.token")
        };
        assert_eq!(run(&unread, now), Err(MintError::SecretUnreadable));
        assert!(!unread.out.exists());
        std::fs::write(dir.path().join("blank"), " \n").unwrap();
        let blank = TokenArgs {
            jwt_secret_file: dir.path().join("blank"),
            ..args("member9", false, "blank.token")
        };
        assert_eq!(run(&blank, now), Err(MintError::SecretUnreadable));
        assert!(!blank.out.exists());
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_at_out_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let secret_file = dir.path().join("jwt_secret");
        std::fs::write(&secret_file, SECRET).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "zyxkept").unwrap();
        let link = dir.path().join("link.token");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // A dangling one too: create_new never follows it.
        let dangling = dir.path().join("dangling.token");
        let nowhere = dir.path().join("nowhere");
        std::os::unix::fs::symlink(&nowhere, &dangling).unwrap();
        for out in [link, dangling] {
            let args = TokenArgs {
                jwt_secret_file: secret_file.clone(),
                sub: "member9".to_owned(),
                engineer: false,
                seconds: 60,
                out,
            };
            assert_eq!(run(&args, unix_now()), Err(MintError::TokenUnwritable));
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "zyxkept");
        assert!(!nowhere.exists());
    }

    #[test]
    fn the_clock_gives_whole_unix_seconds_and_refuses_one_before_1970() {
        let at = |s: u64, ms: u64| UNIX_EPOCH + Duration::from_secs(s) + Duration::from_millis(ms);
        assert_eq!(unix_seconds(UNIX_EPOCH), Ok(0));
        assert_eq!(unix_seconds(at(1_700_000_000, 999)), Ok(1_700_000_000));
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_seconds(before), Err(MintError::ClockUnreadable));
    }

    #[test]
    fn the_failures_are_fixed_codes() {
        assert_eq!(MintError::SecretUnreadable.code(), "secret-unreadable");
        assert_eq!(MintError::TokenUnwritable.code(), "token-unwritable");
        assert_eq!(MintError::ClockUnreadable.code(), "clock-unreadable");
        assert_eq!(WRITTEN, "token-written");
        assert!(USAGE.starts_with("iem-soakclient token --jwt-secret-file"));
    }
}
