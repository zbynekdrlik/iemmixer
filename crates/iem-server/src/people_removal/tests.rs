//! Removing a person (#58, the owner's ruling of 2026-10-24).

use super::*;

use crate::pin_store::{ENGINEER_ID, PIN_HASHES_FILE};

/// A PHC string of the right kind (the store checks the prefix only).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo";

fn lineup_file(dir: &Path, lineup: &str, kind: &str, id: &str) -> PathBuf {
    dir.join(LINEUPS_DIR)
        .join(lineup)
        .join(kind)
        .join(format!("{id}.json"))
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn store(dir: &Path) -> PinStore {
    PinStore::load(&dir.join("secrets")).unwrap()
}

/// Every file under `dir` with its bytes.
fn all_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(all_files(&path));
        } else {
            let bytes = fs::read(&path).unwrap();
            out.push((path, bytes));
        }
    }
    out
}

fn holds(bytes: &[u8], text: &str) -> bool {
    bytes.windows(text.len()).any(|w| w == text.as_bytes())
}

#[test]
fn a_removal_deletes_the_pin_hash_the_photo_and_every_lineups_files() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files = [
        lineup_file(d, "main", "presets", "member5"),
        lineup_file(d, "main", "customizations", "member5"),
        lineup_file(d, "youth", "snapshots", "member5"),
    ];
    for f in &files {
        write(f, "member5's data");
    }
    let other = lineup_file(d, "main", "presets", "member6");
    write(&other, "member6's");
    write(&d.join(LINEUPS_DIR).join("active.json"), "{}");
    // A directory that is no lineup's is never walked, nor a file named
    // like a lineup.
    let stray = lineup_file(d, "Not A Lineup", "presets", "member5");
    write(&stray, "stray");
    write(&d.join(LINEUPS_DIR).join("youth2"), "a file");
    let photos = PhotoStore::new(d);
    photos.save("member5", b"member5's photo").unwrap();
    photos.save("member6", b"member6's photo").unwrap();
    let mut pins = store(d);
    pins.set_member_hash("member5", PHC.into()).unwrap();
    pins.set_member_hash("member6", PHC.into()).unwrap();

    assert!(remove_person(d, &mut pins, "member5").unwrap());
    for f in &files {
        assert!(!f.exists(), "{}", f.display());
    }
    assert!(!photos.exists("member5"));
    assert_eq!(pins.member_hash("member5"), None);
    // Nothing of theirs is kept anywhere under the config directory.
    for (path, bytes) in all_files(d) {
        for theirs in ["member5's data", "member5's photo", "\"member5\""] {
            assert!(!holds(&bytes, theirs), "{}: {theirs}", path.display());
        }
    }
    // Nobody else's.
    assert_eq!(fs::read_to_string(other).unwrap(), "member6's");
    assert_eq!(fs::read_to_string(stray).unwrap(), "stray");
    assert_eq!(photos.load("member6").unwrap(), b"member6's photo");
    assert_eq!(pins.member_hash("member6"), Some(PHC));
    // Again: nothing of theirs is left.
    assert!(!remove_person(d, &mut pins, "member5").unwrap());
}

/// A PIN hash alone, a photo alone or files alone each make a removal;
/// nothing of theirs is none, and writes nothing.
#[test]
fn any_part_alone_is_a_removal_and_nothing_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut pins = store(d);
    pins.set_member_hash("member4", PHC.into()).unwrap();
    PhotoStore::new(d).save("member5", b"jpeg").unwrap();
    write(&lineup_file(d, "youth", "presets", "member6"), "member6's");
    for id in ["member4", "member5", "member6"] {
        assert!(remove_person(d, &mut pins, id).unwrap(), "{id}");
    }
    assert_eq!(pins.member_hash("member4"), None);
    assert!(!PhotoStore::new(d).exists("member5"));
    assert!(!lineup_file(d, "youth", "presets", "member6").exists());
    // Nobody: no PIN, no photo, no file, not even a lineups directory.
    let bare = tempfile::tempdir().unwrap();
    let mut none = store(bare.path());
    assert!(!remove_person(bare.path(), &mut none, "member7").unwrap());
    assert!(!bare.path().join("secrets").exists(), "no PIN file written");
    assert!(!bare.path().join(LINEUPS_DIR).exists());
}

/// A photo or a file that cannot be deleted is an error that names no
/// path; the PIN hash went first and every other part is deleted all the
/// same.
#[test]
fn what_cannot_be_deleted_is_an_error_and_the_rest_goes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut pins = store(d);
    pins.set_member_hash("member5", PHC.into()).unwrap();
    // A directory where member5's presets file of main is.
    let blocked = lineup_file(d, "main", "presets", "member5");
    write(&blocked.join("inside"), "in the way");
    let rest = lineup_file(d, "youth", "snapshots", "member5");
    write(&rest, "member5's");
    PhotoStore::new(d).save("member5", b"jpeg").unwrap();
    let err = remove_person(d, &mut pins, "member5").unwrap_err();
    assert!(!err.to_string().contains(&*d.to_string_lossy()), "{err}");
    assert_eq!(pins.member_hash("member5"), None, "no login");
    assert!(!rest.exists());
    assert!(!PhotoStore::new(d).exists("member5"));
    assert!(blocked.exists());
    // A directory where member6's photo is: their files still go.
    write(&d.join("photos").join("member6.jpg").join("inside"), "x");
    let theirs = lineup_file(d, "main", "snapshots", "member6");
    write(&theirs, "member6's");
    assert!(remove_person(d, &mut pins, "member6").is_err());
    assert!(!theirs.exists());
    // A lineups folder that cannot be read is an error, never nobody.
    let odd = tempfile::tempdir().unwrap();
    write(&odd.path().join(LINEUPS_DIR), "a file");
    let mut none = store(odd.path());
    assert!(remove_person(odd.path(), &mut none, "member7").is_err());
}

#[test]
fn the_engineer_and_bad_ids_are_refused_before_anything_goes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut pins = store(d);
    pins.set_engineer_hash(PHC.into()).unwrap();
    let engineers = lineup_file(d, "main", "presets", ENGINEER_ID);
    write(&engineers, "the engineer's");
    PhotoStore::new(d).save(ENGINEER_ID, b"jpeg").unwrap();
    for bad in [ENGINEER_ID, "../x", ""] {
        let err = remove_person(d, &mut pins, bad).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
    assert_eq!(pins.engineer_hash(), Some(PHC));
    assert_eq!(fs::read_to_string(engineers).unwrap(), "the engineer's");
    assert!(PhotoStore::new(d).exists(ENGINEER_ID));
    assert!(d.join("secrets").join(PIN_HASHES_FILE).exists());
}
