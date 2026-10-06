//! Durable renames (#32 minor-7): the engine's state files are committed by
//! renames, and a save reported `Saved` must survive a power cut. Windows
//! has no directory handle to flush after a rename, so the rename itself
//! goes to the disk before it returns.

use std::io;
use std::path::Path;

/// Renames `from` to `to`, replacing `to` if it exists, and returns once
/// the move is on the disk: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`. Both paths lie in one directory (the state
/// directory), so the move is a rename, never a copy. `Unsupported` off
/// Windows, where `rename(2)` and a directory fsync do the same.
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    imp::rename_durable(from, to)
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn rename_durable(_from: &Path, _to: &Path) -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    /// `p` as NUL-terminated UTF-16.
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (wide(from), wide(to));
        // SAFETY: two NUL-terminated UTF-16 paths that outlive the call.
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory of this test under the temp directory.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("iem-win-file-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    #[test]
    fn a_durable_rename_replaces_the_target() {
        let dir = scratch("replace");
        let (from, to) = (dir.join("save.tmp"), dir.join("current.json"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        rename_durable(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        // A missing source is the OS error, nothing renamed.
        let e = rename_durable(&from, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_durable_rename_replaces_a_target_another_process_holds_with_delete_sharing() {
        // #32 F3-r4 5: a virus scanner or the indexer may hold the target
        // (baseline.json, save.tmp) open with delete sharing. MoveFileExW
        // then fails with ERROR_ACCESS_DENIED, as std's rename once did
        // before it learnt to retry with POSIX semantics: the save failed.
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let dir = scratch("held");
        let (from, to) = (dir.join("save.new"), dir.join("save.tmp"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&to)
            .unwrap();
        rename_durable(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        drop(held);
        // A missing source still fails, nothing renamed.
        let e = rename_durable(&from, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_a_durable_rename_is_unsupported() {
        let dir = scratch("unsupported");
        let (from, to) = (dir.join("save.tmp"), dir.join("current.json"));
        std::fs::write(&from, b"new").unwrap();
        assert_eq!(
            crate::kind(rename_durable(&from, &to)),
            Some(io::ErrorKind::Unsupported)
        );
        assert!(from.exists() && !to.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
