//! `HKEY_CURRENT_USER` values that keep their kind (S6 design note §3: the
//! preference window writes 32 with the original value's kind and restores
//! the original, reading each write back), and [`HkcuPref`], the preference
//! store of [`crate::prefwin`] on the PC. `HKEY_LOCAL_MACHINE` is read only
//! ([`Hklm`]: the guard's drift check reads service start types, design
//! §5.1).

use std::io;

use crate::prefwin::{Pref, PrefStore};

pub use crate::prefwin::Kind;

/// Values under `HKEY_CURRENT_USER`.
#[derive(Clone, Copy, Debug)]
pub struct Hkcu;

impl Hkcu {
    /// Reads `name` under `key`: its kind and its value as text (a DWORD in
    /// decimal). Any other kind is `InvalidData`.
    pub fn read(key: &str, name: &str) -> io::Result<(Kind, String)> {
        imp::read(key, name)
    }

    /// Writes `value` as `kind` to `name` under the existing `key` (a missing
    /// key is `NotFound`; it is never created). A DWORD's text must be a
    /// decimal `u32` (`InvalidInput`).
    pub fn write(key: &str, name: &str, kind: Kind, value: &str) -> io::Result<()> {
        match kind {
            Kind::Dword => imp::write_dword(key, name, crate::decide::dword(value)?),
            Kind::Text => imp::write_text(key, name, value),
        }
    }
}

/// Values under `HKEY_LOCAL_MACHINE`, read only (a Limited user reads them;
/// nothing here ever writes the machine's settings).
#[derive(Clone, Copy, Debug)]
pub struct Hklm;

impl Hklm {
    /// Reads `name` under `key` like [`Hkcu::read`]: its kind and its value
    /// as text (a DWORD in decimal); any other kind is `InvalidData`.
    pub fn read(key: &str, name: &str) -> io::Result<(Kind, String)> {
        imp::read_machine(key, name)
    }
}

/// The driver's preferred-buffer value under `HKEY_CURRENT_USER` (key and
/// value name from the site's `[card]`), as the [`PrefStore`] of
/// [`crate::prefwin`]. Errors name the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HkcuPref {
    pub key: String,
    pub name: String,
}

impl HkcuPref {
    fn failed(&self, what: &str, e: &io::Error) -> String {
        format!("{what} HKCU\\{}\\{}: {e}", self.key, self.name)
    }
}

impl PrefStore for HkcuPref {
    fn read(&mut self) -> Result<Pref, String> {
        Hkcu::read(&self.key, &self.name)
            .map(|(kind, raw)| Pref { kind, raw })
            .map_err(|e| self.failed("reading", &e))
    }

    fn write(&mut self, value: &Pref) -> Result<(), String> {
        Hkcu::write(&self.key, &self.name, value.kind, &value.raw)
            .map_err(|e| self.failed("writing", &e))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    use super::Kind;

    pub(super) fn read(_key: &str, _name: &str) -> io::Result<(Kind, String)> {
        crate::unsupported()
    }

    pub(super) fn read_machine(_key: &str, _name: &str) -> io::Result<(Kind, String)> {
        crate::unsupported()
    }

    pub(super) fn write_dword(_key: &str, _name: &str, _value: u32) -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn write_text(_key: &str, _name: &str, _value: &str) -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;

    use windows_registry::{CURRENT_USER, Key, LOCAL_MACHINE, Type};

    use super::Kind;

    pub(super) fn read(key: &str, name: &str) -> io::Result<(Kind, String)> {
        read_in(CURRENT_USER, key, name)
    }

    pub(super) fn read_machine(key: &str, name: &str) -> io::Result<(Kind, String)> {
        read_in(LOCAL_MACHINE, key, name)
    }

    /// `Key::open` asks for read access only.
    fn read_in(root: &Key, key: &str, name: &str) -> io::Result<(Kind, String)> {
        let value = root.open(key)?.get_value(name)?;
        match value.ty() {
            Type::U32 => Ok((Kind::Dword, u32::try_from(value)?.to_string())),
            Type::String => Ok((Kind::Text, String::try_from(value)?)),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{key}\\{name} is {other:?}, neither a DWORD nor a string"),
            )),
        }
    }

    /// Opens an existing key for reading and writing (never creates it).
    fn writable(key: &str) -> io::Result<Key> {
        CURRENT_USER
            .options()
            .read()
            .write()
            .open(key)
            .map_err(io::Error::from)
    }

    pub(super) fn write_dword(key: &str, name: &str, value: u32) -> io::Result<()> {
        writable(key)?.set_u32(name, value).map_err(io::Error::from)
    }

    pub(super) fn write_text(key: &str, name: &str, value: &str) -> io::Result<()> {
        writable(key)?
            .set_string(name, value)
            .map_err(io::Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;

    #[test]
    fn a_dword_must_be_decimal_before_anything_is_written() {
        for bad in ["", "+32", "x32", "-1", "4294967296", "0x20", "32 "] {
            assert_eq!(
                kind(Hkcu::write("Software\\Test", "Frames", Kind::Dword, bad)),
                Some(io::ErrorKind::InvalidInput),
                "{bad:?}"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn the_registry_is_unsupported_off_windows() {
        use std::io::ErrorKind::Unsupported;

        assert_eq!(
            kind(Hkcu::read("Software\\Test", "Frames")),
            Some(Unsupported)
        );
        assert_eq!(kind(Hklm::read("SYSTEM\\Test", "Start")), Some(Unsupported));
        assert_eq!(
            kind(Hkcu::write("Software\\Test", "Frames", Kind::Dword, "32")),
            Some(Unsupported)
        );
        assert_eq!(
            kind(Hkcu::write(
                "Software\\Test",
                "Name",
                Kind::Text,
                "Test Card"
            )),
            Some(Unsupported)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_preference_store_names_the_value_it_failed_on() {
        let mut store = HkcuPref {
            key: "Software\\Test".into(),
            name: "Frames".into(),
        };
        assert_eq!(
            store.read(),
            Err("reading HKCU\\Software\\Test\\Frames: Windows only".to_string())
        );
        let pref = Pref {
            kind: Kind::Dword,
            raw: "32".into(),
        };
        assert_eq!(
            store.write(&pref),
            Err("writing HKCU\\Software\\Test\\Frames: Windows only".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn machine_values_are_read_with_their_kind() {
        // The event log service starts automatically (2) on every Windows.
        assert_eq!(
            Hklm::read("SYSTEM\\CurrentControlSet\\Services\\EventLog", "Start").unwrap(),
            (Kind::Dword, "2".to_string())
        );
        assert_eq!(
            kind(Hklm::read(
                "SYSTEM\\CurrentControlSet\\Services\\EventLog",
                "iemmixer-missing"
            )),
            Some(io::ErrorKind::NotFound)
        );
    }

    /// A key of this test only, removed when the test ends (passing or not).
    #[cfg(windows)]
    struct TestKey(String);

    #[cfg(windows)]
    impl Drop for TestKey {
        fn drop(&mut self) {
            let _ = windows_registry::CURRENT_USER.remove_tree(&self.0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn values_round_trip_with_their_kind() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use windows_registry::{CURRENT_USER, Type};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = TestKey(format!(
            "Software\\iemmixer-test-{}-{nanos}",
            std::process::id()
        ));
        let raw = CURRENT_USER.create(&key.0).unwrap();

        Hkcu::write(&key.0, "Frames", Kind::Dword, "32").unwrap();
        assert_eq!(
            Hkcu::read(&key.0, "Frames").unwrap(),
            (Kind::Dword, "32".to_string())
        );
        assert_eq!(raw.get_type("Frames").unwrap(), Type::U32);
        Hkcu::write(&key.0, "Frames", Kind::Dword, "64").unwrap();
        assert_eq!(raw.get_u32("Frames").unwrap(), 64);

        Hkcu::write(&key.0, "Size", Kind::Text, "32").unwrap();
        assert_eq!(
            Hkcu::read(&key.0, "Size").unwrap(),
            (Kind::Text, "32".to_string())
        );
        assert_eq!(raw.get_type("Size").unwrap(), Type::String);

        raw.set_bytes("Raw", Type::Bytes, &[1, 2]).unwrap();
        assert_eq!(
            kind(Hkcu::read(&key.0, "Raw")),
            Some(io::ErrorKind::InvalidData)
        );
        assert_eq!(
            kind(Hkcu::read(&key.0, "Missing")),
            Some(io::ErrorKind::NotFound)
        );
        let missing = format!("{}\\missing", key.0);
        assert_eq!(
            kind(Hkcu::write(&missing, "Frames", Kind::Dword, "32")),
            Some(io::ErrorKind::NotFound)
        );
        assert!(CURRENT_USER.open(&missing).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_preference_window_opens_and_closes_on_the_registry() {
        use crate::prefwin::{self, Pref, PrefError};
        use std::time::{SystemTime, UNIX_EPOCH};
        use windows_registry::CURRENT_USER;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = TestKey(format!(
            "Software\\iemmixer-test-pref-{}-{nanos}",
            std::process::id()
        ));
        let raw = CURRENT_USER.create(&key.0).unwrap();
        raw.set_u32("Frames", 64).unwrap();
        let original = Pref {
            kind: Kind::Dword,
            raw: "64".into(),
        };
        let mut store = HkcuPref {
            key: key.0.clone(),
            name: "Frames".into(),
        };

        prefwin::enter(&mut store, &original, 32).unwrap();
        assert_eq!(raw.get_u32("Frames").unwrap(), 32);
        assert_eq!(
            prefwin::enter(&mut store, &original, 32),
            Err(PrefError::NotOriginal {
                found: Pref {
                    kind: Kind::Dword,
                    raw: "32".into()
                }
            })
        );
        prefwin::leave(&mut store, &original).unwrap();
        assert_eq!(raw.get_u32("Frames").unwrap(), 64);
        assert_eq!(prefwin::restore(&mut store, &original, 3), Ok(0));

        let mut missing = HkcuPref {
            key: key.0.clone(),
            name: "Missing".into(),
        };
        assert!(matches!(
            prefwin::enter(&mut missing, &original, 32),
            Err(PrefError::Read(_))
        ));
    }
}
