//! `HKEY_CURRENT_USER` values that keep their kind (S6 design note §3: the
//! preference window writes 32 with the original value's kind and restores
//! the original, reading each write back).

use std::io;

/// The kind of a registry value; a write names it, so a value read as a
/// DWORD is written back as a DWORD and a string as a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `REG_DWORD`; its text is decimal.
    Dword,
    /// `REG_SZ`.
    Text,
}

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
            Kind::Dword => imp::write_dword(key, name, dword(value)?),
            Kind::Text => imp::write_text(key, name, value),
        }
    }
}

fn dword(text: &str) -> io::Result<u32> {
    text.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a decimal DWORD: {text:?}"),
        )
    })
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    use super::Kind;

    pub(super) fn read(_key: &str, _name: &str) -> io::Result<(Kind, String)> {
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

    use windows_registry::{CURRENT_USER, Key, Type};

    use super::Kind;

    pub(super) fn read(key: &str, name: &str) -> io::Result<(Kind, String)> {
        let value = CURRENT_USER.open(key)?.get_value(name)?;
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
        for bad in ["", "x32", "-1", "4294967296", "0x20", "32 "] {
            assert_eq!(
                kind(Hkcu::write("Software\\Test", "Frames", Kind::Dword, bad)),
                Some(io::ErrorKind::InvalidInput),
                "{bad:?}"
            );
        }
        assert_eq!(dword("4294967295").unwrap(), u32::MAX);
        assert_eq!(dword("0").unwrap(), 0);
        assert_eq!(dword("32").unwrap(), 32);
    }

    #[cfg(not(windows))]
    #[test]
    fn the_registry_is_unsupported_off_windows() {
        use std::io::ErrorKind::Unsupported;

        assert_eq!(
            kind(Hkcu::read("Software\\Test", "Frames")),
            Some(Unsupported)
        );
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
}
