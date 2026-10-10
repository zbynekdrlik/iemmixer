//! Installing and activating CI bundles (S6 design note §5.5; plan Task 10
//! Step 3; P5/G8).
//!
//! `iemmixer-guard install <zip>` and the guard's `Install` unpack a bundle
//! into `bundles\<sha>.partial` (the SHA from the zip's `manifest.json`),
//! verify it against its `SHA256SUMS` and manifest (`bundle::verify`) and
//! rename it to `bundles\<sha>`. An installed SHA is never overwritten: the
//! same sums again are a no-op, other sums are refused. `--verify-only` (the
//! CI's check of every zip) unpacks into a fresh temp directory, verifies
//! and deletes only that directory. `activate` copies the guard and
//! `iemmode` into `bin\`, so their paths never change.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zip::ZipArchive;

use crate::bundle::{self, MANIFEST, Manifest, SUMS, TUNING_DIR};
use crate::site;

/// The guard's exe in `bin\` and in a bundle.
pub const GUARD_EXE: &str = "iemmixer-guard.exe";
/// `iemmode`'s exe in `bin\` and in a bundle.
pub const IEMMODE_EXE: &str = "iemmode.exe";
/// What runs from `bin\` (design §5.5).
pub const BIN_EXES: [&str; 2] = [GUARD_EXE, IEMMODE_EXE];
/// A replaced exe is kept as `<exe>.old-<sha>` until the next start.
pub const OLD_MARK: &str = ".old-";

pub fn bundles_dir(root: &Path) -> PathBuf {
    root.join("bundles")
}

pub fn bin_dir(root: &Path) -> PathBuf {
    root.join("bin")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The zip or what it holds is refused; nothing was installed.
    Refused(String),
    /// The SHA is installed with other sums: never overwritten (an alarm).
    Conflict(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) | Self::Conflict(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for InstallError {}

/// An install's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub manifest: Manifest,
    /// False: the same bundle was installed already (nothing changed).
    pub fresh: bool,
}

fn open(zip: &Path) -> Result<ZipArchive<File>, String> {
    let file = File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    ZipArchive::new(file).map_err(|e| format!("{}: {e}", zip.display()))
}

/// The zip's `manifest.json`: its SHA names the bundle's directory.
fn zip_manifest(archive: &mut ZipArchive<File>) -> Result<Manifest, String> {
    let mut entry = archive
        .by_name(MANIFEST)
        .map_err(|e| format!("{MANIFEST}: {e}"))?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| format!("{MANIFEST}: {e}"))?;
    let m = Manifest::parse(&text)?;
    if bundle::valid_sha(&m.sha) {
        Ok(m)
    } else {
        Err(format!("{MANIFEST} names {:?}, not a commit SHA", m.sha))
    }
}

/// Unpacks every entry into `into` (created). Only the names a bundle may
/// hold (`bundle::valid_name`) and the `tuning/` directory; no links, no
/// entry twice; `enclosed_name` keeps every path inside `into`.
fn extract(archive: &mut ZipArchive<File>, into: &Path) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        let name = entry.name().to_owned();
        if entry.is_symlink() {
            return Err(format!("{name:?}: a link is refused"));
        }
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| format!("{name:?}: leaves the bundle"))?;
        if entry.is_dir() {
            if name.trim_end_matches('/') != TUNING_DIR {
                return Err(format!("{name:?}: the only directory is {TUNING_DIR}/"));
            }
            fs::create_dir_all(into.join(rel)).map_err(|e| format!("{name}: {e}"))?;
            continue;
        }
        if !bundle::valid_name(&name) {
            return Err(format!("{name:?}: not a bundle file name"));
        }
        let path = into.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{name}: {e}"))?;
        }
        let mut out = File::create_new(&path).map_err(|e| format!("{name}: {e}"))?;
        io::copy(&mut entry, &mut out).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(())
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), &rel, out)?;
        } else {
            out.push((rel, bundle::sha256_read(File::open(entry.path())?)?));
        }
    }
    Ok(())
}

/// Every file under `dir` with its SHA-256; names relative and
/// `/`-separated, sorted.
pub fn digests(dir: &Path) -> io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// The parsed `SHA256SUMS` of an unpacked bundle (a UTF-8 byte-order mark
/// is skipped).
fn read_sums(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let text = fs::read_to_string(dir.join(SUMS)).map_err(|e| format!("{SUMS}: {e}"))?;
    bundle::parse_sums(text.strip_prefix('\u{feff}').unwrap_or(&text))
}

/// Checks the unpacked bundle in `dir` for the commit `sha`: every file
/// summed and matching, the required ones there, the manifest naming `sha`.
pub fn verify_dir(dir: &Path, sha: &str) -> Result<(), String> {
    let sums = read_sums(dir)?;
    let text = fs::read_to_string(dir.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let manifest = Manifest::parse(&text)?;
    let files = digests(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    bundle::verify(&files, &sums, &manifest, sha).map_err(|bad| bad.join("; "))
}

/// The same sums file (as parsed, in any order) in both directories.
fn same_sums(a: &Path, b: &Path) -> bool {
    match (read_sums(a), read_sums(b)) {
        (Ok(mut x), Ok(mut y)) => {
            x.sort();
            y.sort();
            x == y
        }
        _ => false,
    }
}

fn remove_dir(dir: &Path) {
    if dir.exists()
        && let Err(e) = fs::remove_dir_all(dir)
    {
        tracing::warn!("removing {}: {e}", dir.display());
    }
}

/// Installs the bundle `zip` under `bundles` (see the module doc).
pub fn install(bundles: &Path, zip: &Path) -> Result<Installed, InstallError> {
    let refused = InstallError::Refused;
    let mut archive = open(zip).map_err(refused)?;
    let manifest = zip_manifest(&mut archive).map_err(refused)?;
    let sha = manifest.sha.clone();
    fs::create_dir_all(bundles)
        .map_err(|e| InstallError::Refused(format!("{}: {e}", bundles.display())))?;
    let partial = bundles.join(format!("{sha}.partial"));
    // A leftover of an interrupted install: our own directory.
    remove_dir(&partial);
    let checked = extract(&mut archive, &partial).and_then(|()| verify_dir(&partial, &sha));
    if let Err(why) = checked {
        remove_dir(&partial);
        return Err(InstallError::Refused(format!("bundle {sha}: {why}")));
    }
    let target = bundles.join(&sha);
    if target.exists() {
        let same = same_sums(&target, &partial);
        remove_dir(&partial);
        return if same {
            Ok(Installed {
                manifest,
                fresh: false,
            })
        } else {
            Err(InstallError::Conflict(format!(
                "bundle {sha} is installed with other sums; it is never overwritten"
            )))
        };
    }
    fs::rename(&partial, &target).map_err(|e| {
        remove_dir(&partial);
        InstallError::Refused(format!("bundle {sha}: {e}"))
    })?;
    Ok(Installed {
        manifest,
        fresh: true,
    })
}

/// CI's check (Task 12): unpacks `zip` into `scratch` (created; it must not
/// exist), verifies it, and removes `scratch` whatever happened.
pub fn verify_only(zip: &Path, scratch: &Path) -> Result<Manifest, String> {
    fs::create_dir(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let checked = unpack_and_verify(zip, scratch);
    remove_dir(scratch);
    checked
}

fn unpack_and_verify(zip: &Path, scratch: &Path) -> Result<Manifest, String> {
    let mut archive = open(zip)?;
    let manifest = zip_manifest(&mut archive)?;
    let dir = scratch.join(&manifest.sha);
    extract(&mut archive, &dir)?;
    verify_dir(&dir, &manifest.sha)?;
    Ok(manifest)
}

fn digest_of(path: &Path) -> Option<String> {
    File::open(path)
        .ok()
        .and_then(|f| bundle::sha256_read(f).ok())
}

/// How many names `<exe>.old-<sha>[.<n>]` an activation tries for a
/// replaced exe before it gives up.
const OLD_NAMES: usize = 16;

/// Copies the guard and `iemmode` of the bundle in `bundle_dir` into `bin`.
/// An exe already equal to the bundle's stays untouched (it may be the
/// running guard's). Any other present exe goes aside first (Windows renames
/// a running exe but never replaces it), under the first of
/// `<exe>.old-<sha>`, `<exe>.old-<sha>.1`, … that is free or can be deleted:
/// an old copy still running never blocks. Returns whether the guard's exe
/// changed (the guard then hands over to the new one).
pub fn activate_bins(bundle_dir: &Path, bin: &Path, sha: &str) -> Result<bool, String> {
    fs::create_dir_all(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let before = digest_of(&bin.join(GUARD_EXE));
    for exe in BIN_EXES {
        let src = bundle_dir.join(exe);
        if !src.is_file() {
            return Err(format!("{}: missing", src.display()));
        }
        let want = digest_of(&src).ok_or_else(|| format!("{}: unreadable", src.display()))?;
        let dst = bin.join(exe);
        if dst.exists() {
            if digest_of(&dst).as_ref() == Some(&want) {
                continue;
            }
            let old = free_old_name(bin, exe, sha)?;
            fs::rename(&dst, &old).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
        fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    let after = digest_of(&bin.join(GUARD_EXE));
    Ok(before != after)
}

/// The first of `<exe>.old-<sha>`, `<exe>.old-<sha>.1`, … in `bin` that is
/// free, deleting a present one when it can.
fn free_old_name(bin: &Path, exe: &str, sha: &str) -> Result<PathBuf, String> {
    let mut last = String::new();
    for n in 0..OLD_NAMES {
        let name = if n == 0 {
            format!("{exe}{OLD_MARK}{sha}")
        } else {
            format!("{exe}{OLD_MARK}{sha}.{n}")
        };
        let old = bin.join(name);
        if !old.exists() {
            return Ok(old);
        }
        match fs::remove_file(&old) {
            Ok(()) => return Ok(old),
            Err(e) => last = format!("{}: {e}", old.display()),
        }
    }
    Err(last)
}

/// Deletes the replaced exes (`*.old-*`) in `bin` at a start; returns the
/// ones that could not be deleted.
pub fn clean_old_bins(bin: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(bin) else {
        return Vec::new();
    };
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(OLD_MARK)
            && let Err(e) = fs::remove_file(entry.path())
        {
            failed.push(format!("{name}: {e}"));
        }
    }
    failed
}

#[derive(Deserialize)]
struct PcRoot {
    root: PathBuf,
}

/// The guard's root folder: `pc.toml`'s `root` under `local_app_data`, or
/// `<local_app_data>\iemmixer` when `pc.toml` is missing or unreadable (the
/// first bundle arrives before the site does).
pub fn root_dir(local_app_data: &Path) -> PathBuf {
    fs::read_to_string(site::pc_toml_path(local_app_data))
        .ok()
        .and_then(|text| toml::from_str::<PcRoot>(&text).ok())
        .map_or_else(|| local_app_data.join("iemmixer"), |p| p.root)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;
    use crate::bundle::{REQUIRED, sha256_hex};

    pub(crate) const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// The files of a complete synthetic bundle for `sha`, its manifest as
    /// the CI `bundle` job writes it (S8 lane 5: `guard_lifecycle`).
    pub(crate) fn files(sha: &str) -> Vec<(String, Vec<u8>)> {
        files_with(
            sha,
            &format!(
                r#"{{"sha":"{sha}","branch":"dev","version":"2.0.0-dev.9","run":4242,"guard_lifecycle":1}}"#
            ),
        )
    }

    /// The files of a bundle built before S8 lane 5: its manifest names no
    /// `guard_lifecycle`.
    pub(crate) fn older_files(sha: &str) -> Vec<(String, Vec<u8>)> {
        files_with(
            sha,
            &format!(r#"{{"sha":"{sha}","branch":"dev","version":"2.0.0-dev.9","run":4242}}"#),
        )
    }

    fn files_with(sha: &str, manifest: &str) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = REQUIRED
            .iter()
            .filter(|n| **n != MANIFEST)
            .map(|n| ((*n).to_owned(), format!("{n} of {sha}").into_bytes()))
            .collect();
        out.push((MANIFEST.to_owned(), manifest.as_bytes().to_vec()));
        out.push((
            "tuning/IemTuning.psm1".to_owned(),
            b"tuning module".to_vec(),
        ));
        out
    }

    /// `SHA256SUMS` for `files`.
    pub(crate) fn sums(files: &[(String, Vec<u8>)]) -> String {
        files
            .iter()
            .map(|(n, b)| format!("{}  {n}\n", sha256_hex(b)))
            .collect()
    }

    /// Writes a zip of `entries` (a trailing `/` makes a directory).
    pub(crate) fn zip_of(path: &Path, entries: &[(String, Vec<u8>)]) {
        let mut z = ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in entries {
            if name.ends_with('/') {
                z.add_directory(name.as_str(), opts).unwrap();
            } else {
                z.start_file(name.as_str(), opts).unwrap();
                z.write_all(bytes).unwrap();
            }
        }
        z.finish().unwrap();
    }

    /// A complete, correctly summed bundle zip for `sha`.
    pub(crate) fn good_zip(dir: &Path, sha: &str) -> PathBuf {
        summed_zip(dir, sha, files(sha))
    }

    /// The same, built before S8 lane 5 (no `guard_lifecycle`).
    pub(crate) fn older_zip(dir: &Path, sha: &str) -> PathBuf {
        summed_zip(dir, sha, older_files(sha))
    }

    fn summed_zip(dir: &Path, sha: &str, mut entries: Vec<(String, Vec<u8>)>) -> PathBuf {
        let text = sums(&entries);
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let path = dir.join(format!("{sha}.zip"));
        zip_of(&path, &entries);
        path
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut n: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        n.sort();
        n
    }

    #[test]
    fn the_happy_path_installs_under_the_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let zip = good_zip(tmp.path(), SHA);
        let got = install(&bundles, &zip).unwrap();
        assert!(got.fresh);
        assert_eq!(got.manifest.sha, SHA);
        assert_eq!(got.manifest.run, 4242);
        assert_eq!(names(&bundles), [SHA]);
        let dir = bundles.join(SHA);
        assert_eq!(
            fs::read(dir.join("iem-engine.exe")).unwrap(),
            format!("iem-engine.exe of {SHA}").into_bytes()
        );
        assert_eq!(
            fs::read(dir.join("tuning").join("IemTuning.psm1")).unwrap(),
            b"tuning module"
        );
        assert_eq!(verify_dir(&dir, SHA), Ok(()));
        // The same zip again changes nothing.
        let again = install(&bundles, &zip).unwrap();
        assert!(!again.fresh);
        assert_eq!(again.manifest, got.manifest);
        assert_eq!(names(&bundles), [SHA]);
    }

    #[test]
    fn a_tampered_file_is_refused_and_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let mut entries = files(SHA);
        let text = sums(&entries);
        if let Some(e) = entries.iter_mut().find(|(n, _)| n == "iem-server.exe") {
            e.1 = b"tampered".to_vec();
        }
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let zip = tmp.path().join("t.zip");
        zip_of(&zip, &entries);
        match install(&bundles, &zip) {
            Err(InstallError::Refused(why)) => {
                assert!(
                    why.starts_with(&format!("bundle {SHA}: iem-server.exe: sha256 ")),
                    "{why}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(names(&bundles).is_empty());
    }

    #[test]
    fn a_traversal_entry_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        for evil in ["../evil.exe", "tuning/../../evil.exe", "bin/evil.exe"] {
            let mut entries = files(SHA);
            entries.push((evil.to_owned(), b"x".to_vec()));
            let text = sums(&entries);
            entries.push((SUMS.to_owned(), text.into_bytes()));
            let zip = tmp.path().join("evil.zip");
            zip_of(&zip, &entries);
            let err = install(&bundles, &zip).unwrap_err();
            assert!(matches!(err, InstallError::Refused(_)), "{evil}: {err:?}");
            assert!(names(&bundles).is_empty(), "{evil}");
            assert!(!tmp.path().join("evil.exe").exists(), "{evil}");
        }
        // A directory other than tuning/ is refused too.
        let mut entries = files(SHA);
        entries.push(("other/".to_owned(), Vec::new()));
        let text = sums(&entries);
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let zip = tmp.path().join("dir.zip");
        zip_of(&zip, &entries);
        assert_eq!(
            install(&bundles, &zip),
            Err(InstallError::Refused(format!(
                "bundle {SHA}: \"other/\": the only directory is tuning/"
            )))
        );
        // tuning/ as its own entry is accepted.
        let mut entries = files(SHA);
        entries.insert(0, ("tuning/".to_owned(), Vec::new()));
        let text = sums(&entries[1..]);
        entries.push((SUMS.to_owned(), text.into_bytes()));
        zip_of(&zip, &entries);
        assert!(install(&bundles, &zip).unwrap().fresh);
    }

    #[test]
    fn a_missing_required_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let mut entries = files(SHA);
        entries.retain(|(n, _)| n != "iemmode.exe");
        let text = sums(&entries);
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let zip = tmp.path().join("m.zip");
        zip_of(&zip, &entries);
        assert_eq!(
            install(&bundles, &zip),
            Err(InstallError::Refused(format!(
                "bundle {SHA}: iemmode.exe: required but not in the bundle"
            )))
        );
        assert!(names(&bundles).is_empty());
    }

    #[test]
    fn a_zip_without_a_valid_manifest_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let zip = tmp.path().join("n.zip");
        zip_of(&zip, &[("a.txt".to_owned(), b"a".to_vec())]);
        let err = install(&bundles, &zip).unwrap_err().to_string();
        assert!(err.starts_with("manifest.json: "), "{err}");
        let mut entries = files("abc");
        entries.retain(|(n, _)| n == MANIFEST);
        zip_of(&zip, &entries);
        assert_eq!(
            install(&bundles, &zip),
            Err(InstallError::Refused(
                "manifest.json names \"abc\", not a commit SHA".into()
            ))
        );
        let missing = tmp.path().join("none.zip");
        assert!(matches!(
            install(&bundles, &missing),
            Err(InstallError::Refused(_))
        ));
        fs::write(&missing, b"not a zip").unwrap();
        assert!(matches!(
            install(&bundles, &missing),
            Err(InstallError::Refused(_))
        ));
    }

    #[test]
    fn an_installed_sha_with_other_sums_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let zip = good_zip(tmp.path(), SHA);
        install(&bundles, &zip).unwrap();
        let mut entries = files(SHA);
        if let Some(e) = entries.iter_mut().find(|(n, _)| n == "hil-v1.ps1") {
            e.1 = b"another hil script".to_vec();
        }
        let text = sums(&entries);
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let other = tmp.path().join("other.zip");
        zip_of(&other, &entries);
        assert_eq!(
            install(&bundles, &other),
            Err(InstallError::Conflict(format!(
                "bundle {SHA} is installed with other sums; it is never overwritten"
            )))
        );
        assert_eq!(names(&bundles), [SHA]);
        assert_eq!(
            fs::read(bundles.join(SHA).join("hil-v1.ps1")).unwrap(),
            format!("hil-v1.ps1 of {SHA}").into_bytes()
        );
        assert_eq!(
            InstallError::Conflict("c".into()).to_string(),
            InstallError::Refused("c".into()).to_string()
        );
    }

    #[test]
    fn a_leftover_partial_directory_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let partial = bundles.join(format!("{SHA}.partial"));
        fs::create_dir_all(&partial).unwrap();
        fs::write(partial.join("stale.exe"), b"old").unwrap();
        let zip = good_zip(tmp.path(), SHA);
        assert!(install(&bundles, &zip).unwrap().fresh);
        assert_eq!(names(&bundles), [SHA]);
        assert!(!bundles.join(SHA).join("stale.exe").exists());
    }

    #[test]
    fn verify_only_leaves_no_bundle_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = good_zip(tmp.path(), SHA);
        let scratch = tmp.path().join("scratch");
        let m = verify_only(&zip, &scratch).unwrap();
        assert_eq!(m.sha, SHA);
        assert!(!scratch.exists());
        assert_eq!(names(tmp.path()), [format!("{SHA}.zip")]);
        // A bad zip is refused and its scratch removed as well.
        let mut entries = files(SHA);
        let text = sums(&entries);
        entries.retain(|(n, _)| n != "iem-tray.exe");
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let bad = tmp.path().join("bad.zip");
        zip_of(&bad, &entries);
        assert_eq!(
            verify_only(&bad, &scratch),
            Err("iem-tray.exe: summed but missing".to_owned())
        );
        assert!(!scratch.exists());
        // A scratch that exists already is never used (nor removed).
        fs::create_dir(&scratch).unwrap();
        fs::write(scratch.join("keep"), b"k").unwrap();
        assert!(verify_only(&zip, &scratch).is_err());
        assert!(scratch.join("keep").exists());
    }

    #[test]
    fn digests_walk_the_tree_sorted_with_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("tuning")).unwrap();
        fs::write(tmp.path().join("b.exe"), b"b").unwrap();
        fs::write(tmp.path().join("a.exe"), b"a").unwrap();
        fs::write(tmp.path().join("tuning").join("t.psm1"), b"t").unwrap();
        assert_eq!(
            digests(tmp.path()).unwrap(),
            [
                ("a.exe".to_owned(), sha256_hex(b"a")),
                ("b.exe".to_owned(), sha256_hex(b"b")),
                ("tuning/t.psm1".to_owned(), sha256_hex(b"t")),
            ]
        );
    }

    #[test]
    fn sums_with_a_byte_order_mark_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let bundles = tmp.path().join("bundles");
        let mut entries = files(SHA);
        let text = format!("\u{feff}{}", sums(&entries));
        entries.push((SUMS.to_owned(), text.into_bytes()));
        let zip = tmp.path().join("bom.zip");
        zip_of(&zip, &entries);
        assert!(install(&bundles, &zip).unwrap().fresh);
    }

    #[test]
    fn activation_copies_the_exes_and_keeps_the_old_ones_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle_a = tmp.path().join("a");
        let bundle_b = tmp.path().join("b");
        for (dir, tag) in [(&bundle_a, "a"), (&bundle_b, "b")] {
            fs::create_dir_all(dir).unwrap();
            for exe in BIN_EXES {
                fs::write(dir.join(exe), format!("{exe} {tag}")).unwrap();
            }
        }
        let bin = tmp.path().join("bin");
        // The first activation: no guard before, so it changed.
        assert_eq!(activate_bins(&bundle_a, &bin, "a"), Ok(true));
        assert_eq!(names(&bin), [GUARD_EXE, IEMMODE_EXE]);
        // The same bundle again moves nothing: the running guard's exe stays
        // where it is (#9 2026-09-28: HIL's second activation of the active
        // bundle met the first one's renamed, still running exe).
        assert_eq!(activate_bins(&bundle_a, &bin, "a"), Ok(false));
        assert_eq!(names(&bin), [GUARD_EXE, IEMMODE_EXE]);
        assert_eq!(activate_bins(&bundle_b, &bin, "b"), Ok(true));
        assert_eq!(
            fs::read_to_string(bin.join(GUARD_EXE)).unwrap(),
            "iemmixer-guard.exe b"
        );
        assert_eq!(
            fs::read_to_string(bin.join(format!("{IEMMODE_EXE}.old-b"))).unwrap(),
            "iemmode.exe a"
        );
        assert_eq!(clean_old_bins(&bin), Vec::<String>::new());
        assert_eq!(names(&bin), [GUARD_EXE, IEMMODE_EXE]);
        // A bundle without the exes is refused before anything moves.
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let err = activate_bins(&empty, &bin, "c").unwrap_err();
        assert!(err.ends_with("iemmixer-guard.exe: missing"), "{err}");
        assert_eq!(names(&bin), [GUARD_EXE, IEMMODE_EXE]);
        // No bin directory yet: nothing to clean.
        assert!(clean_old_bins(&tmp.path().join("nothing")).is_empty());
    }

    #[test]
    fn an_old_exe_that_cannot_go_never_blocks_an_activation() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("b");
        fs::create_dir_all(&bundle).unwrap();
        for exe in BIN_EXES {
            fs::write(bundle.join(exe), format!("{exe} b")).unwrap();
        }
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        for exe in BIN_EXES {
            fs::write(bin.join(exe), format!("{exe} a")).unwrap();
        }
        // An old copy of the same name that cannot be deleted (on the PC an
        // exe still running from it; here a folder, which no file removal
        // takes): the replaced exe goes aside under the next free name.
        let stuck = bin.join(format!("{GUARD_EXE}.old-b"));
        fs::create_dir_all(stuck.join("x")).unwrap();
        fs::write(bin.join(format!("{GUARD_EXE}.old-b.1")), b"").unwrap();
        assert_eq!(activate_bins(&bundle, &bin, "b"), Ok(true));
        assert_eq!(
            fs::read_to_string(bin.join(GUARD_EXE)).unwrap(),
            "iemmixer-guard.exe b"
        );
        assert!(stuck.join("x").is_dir());
        assert_eq!(
            fs::read_to_string(bin.join(format!("{GUARD_EXE}.old-b.1"))).unwrap(),
            "iemmixer-guard.exe a"
        );
        assert_eq!(
            fs::read_to_string(bin.join(format!("{IEMMODE_EXE}.old-b"))).unwrap(),
            "iemmode.exe a"
        );
    }

    #[test]
    fn the_root_comes_from_pc_toml_or_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(root_dir(tmp.path()), tmp.path().join("iemmixer"));
        let pc = site::pc_toml_path(tmp.path());
        fs::create_dir_all(pc.parent().unwrap()).unwrap();
        fs::write(&pc, "root = 'D:\\IEM'\nsite = 'x'\n").unwrap();
        assert_eq!(root_dir(tmp.path()), PathBuf::from("D:\\IEM"));
        fs::write(&pc, "not toml [").unwrap();
        assert_eq!(root_dir(tmp.path()), tmp.path().join("iemmixer"));
        assert_eq!(bundles_dir(Path::new("r")), Path::new("r").join("bundles"));
        assert_eq!(bin_dir(Path::new("r")), Path::new("r").join("bin"));
    }
}
