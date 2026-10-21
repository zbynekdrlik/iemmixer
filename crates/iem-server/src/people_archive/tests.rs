//! The people archive (#58 decision 6).

use super::*;

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

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn archive_moves_photo_files_and_pin_hash_and_restore_brings_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let files = [
        lineup_file(d, "main", "presets", "member5"),
        lineup_file(d, "main", "customizations", "member5"),
        lineup_file(d, "youth", "snapshots", "member5"),
    ];
    for f in &files {
        write(f, &f.display().to_string());
    }
    let other = lineup_file(d, "main", "presets", "member6");
    write(&other, "member6's");
    write(&d.join(LINEUPS_DIR).join("active.json"), "{}");
    // A directory that is no lineup is not the archive's.
    let stray = lineup_file(d, "Not A Lineup", "presets", "member5");
    write(&stray, "stray");
    PhotoStore::new(d).save("member5", b"jpeg").unwrap();
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    pins.set_member_hash("member5", PHC.into()).unwrap();

    assert!(archive_person(d, &mut pins, "member5").unwrap());
    let home = d.join(ARCHIVE_DIR).join("member5");
    for f in &files {
        assert!(!f.exists(), "{}", f.display());
        let rel = f.strip_prefix(d).unwrap();
        assert_eq!(read(&home.join(rel)), f.display().to_string());
    }
    assert_eq!(fs::read(home.join(PHOTO)).unwrap(), b"jpeg");
    assert!(!PhotoStore::new(d).exists("member5"));
    assert_eq!(pins.member_hash("member5"), None);
    assert_eq!(pins.archived_hash("member5"), Some(PHC));
    assert_eq!(read(&other), "member6's", "another person's files stay");
    assert_eq!(read(&stray), "stray");

    assert!(restore_person(d, &mut pins, "member5").unwrap());
    for f in &files {
        assert_eq!(read(f), f.display().to_string());
    }
    assert_eq!(PhotoStore::new(d).load("member5").unwrap(), b"jpeg");
    assert_eq!(pins.member_hash("member5"), Some(PHC));
    assert!(!home.exists(), "an emptied archive is removed");

    // Archived again; a file made since in its place is kept on restore,
    // and the archived one stays in the archive.
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    write(&files[0], "newer");
    assert!(restore_person(d, &mut pins, "member5").unwrap());
    assert_eq!(read(&files[0]), "newer");
    assert_eq!(read(&files[1]), files[1].display().to_string());
    let kept = home.join(files[0].strip_prefix(d).unwrap());
    assert_eq!(read(&kept), files[0].display().to_string());
    assert!(!home.join(PHOTO).exists(), "the photo came back");

    // Archived once more: the newer file never lands on the older copy, the
    // older copy moves aside under a number.
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    assert_eq!(read(&kept), "newer");
    let aside = |path: &Path, n: usize| {
        let name = path.file_name().unwrap().to_str().unwrap();
        path.with_file_name(format!("{name}.{n}"))
    };
    assert_eq!(read(&aside(&kept, 1)), files[0].display().to_string());
    assert!(!aside(&kept, 2).exists());
    assert!(!aside(&home.join(PHOTO), 1).exists(), "no photo in the way");
}

/// A newer photo is kept at a restore; the next archive moves the older
/// archived one aside. The aside copies stay in the archive.
#[test]
fn an_archived_photo_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let photos = PhotoStore::new(d);
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    photos.save("member5", b"first").unwrap();
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    let home = d.join(ARCHIVE_DIR).join("member5");
    photos.save("member5", b"second").unwrap();
    assert!(
        !restore_person(d, &mut pins, "member5").unwrap(),
        "nothing came back"
    );
    assert_eq!(photos.load("member5").unwrap(), b"second");
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    assert_eq!(fs::read(home.join(PHOTO)).unwrap(), b"second");
    assert_eq!(fs::read(home.join("photo.jpg.1")).unwrap(), b"first");
    photos.save("member5", b"third").unwrap();
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    assert_eq!(fs::read(home.join("photo.jpg.2")).unwrap(), b"second");
    assert_eq!(fs::read(home.join(PHOTO)).unwrap(), b"third");
    // A restore brings the newest back; the older copies stay.
    assert!(restore_person(d, &mut pins, "member5").unwrap());
    assert_eq!(photos.load("member5").unwrap(), b"third");
    assert!(home.join("photo.jpg.1").exists() && home.join("photo.jpg.2").exists());
}

/// A person whose only archived thing is the photo comes back by it alone;
/// one whose photo is kept newer still gets the PIN back.
#[test]
fn a_photo_alone_or_a_pin_alone_brings_a_person_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let photos = PhotoStore::new(d);
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    photos.save("member5", b"jpeg").unwrap();
    assert!(archive_person(d, &mut pins, "member5").unwrap());
    assert!(restore_person(d, &mut pins, "member5").unwrap());
    assert_eq!(photos.load("member5").unwrap(), b"jpeg");

    pins.set_member_hash("member6", PHC.into()).unwrap();
    photos.save("member6", b"old").unwrap();
    assert!(archive_person(d, &mut pins, "member6").unwrap());
    photos.save("member6", b"new").unwrap();
    assert!(
        restore_person(d, &mut pins, "member6").unwrap(),
        "the PIN came back"
    );
    assert_eq!(pins.member_hash("member6"), Some(PHC));
    assert_eq!(photos.load("member6").unwrap(), b"new");
}

/// The PIN hash goes first: an archive that fails half-way leaves no login.
#[test]
fn a_failed_archive_leaves_no_login() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(&lineup_file(d, "main", "presets", "member5"), "presets");
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    pins.set_member_hash("member5", PHC.into()).unwrap();
    // A file where the archive's lineups directory belongs.
    write(
        &d.join(ARCHIVE_DIR).join("member5").join(LINEUPS_DIR),
        "in the way",
    );
    assert!(archive_person(d, &mut pins, "member5").is_err());
    assert_eq!(pins.member_hash("member5"), None, "no login");
    assert_eq!(pins.archived_hash("member5"), Some(PHC));
    assert_eq!(
        read(&lineup_file(d, "main", "presets", "member5")),
        "presets"
    );
}

#[test]
fn archive_of_an_unknown_id_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let other = lineup_file(d, "main", "presets", "member6");
    write(&other, "member6's");
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    assert!(!archive_person(d, &mut pins, "member7").unwrap());
    assert!(!d.join(ARCHIVE_DIR).exists(), "nothing created");
    assert!(!restore_person(d, &mut pins, "member7").unwrap());
    assert!(!d.join("secrets").exists(), "no PIN file written");
    assert_eq!(read(&other), "member6's");
    // Without any lineup either (the legacy layout).
    let bare = tempfile::tempdir().unwrap();
    assert!(!archive_person(bare.path(), &mut pins, "member7").unwrap());
}

#[test]
fn a_pin_alone_is_archived_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut pins = PinStore::load(&d.join("secrets")).unwrap();
    pins.set_member_hash("member4", PHC.into()).unwrap();
    assert!(archive_person(d, &mut pins, "member4").unwrap());
    assert!(!d.join(ARCHIVE_DIR).exists(), "no file to keep");
    assert!(restore_person(d, &mut pins, "member4").unwrap());
    assert_eq!(pins.member_hash("member4"), Some(PHC));
}

#[test]
fn the_engineer_and_bad_ids_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut pins = PinStore::load(&dir.path().join("secrets")).unwrap();
    for bad in [ENGINEER_ID, "../x", ""] {
        let err = archive_person(dir.path(), &mut pins, bad).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        let err = restore_person(dir.path(), &mut pins, bad).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
}
