//! Durable renames (#32 minor-7): the engine's state files are committed by
//! renames, and a save reported `Saved` must survive a power cut. Windows
//! has no directory handle to flush after a rename, so the rename itself
//! goes to the disk before it returns.

use std::io;
use std::path::Path;

/// Renames `from` to `to`, replacing `to` if it exists, and returns once
/// the move is on the disk: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`. Both paths lie in one directory (the state
/// directory), so the move is a rename, never a copy. A target another
/// process holds open with delete sharing (a virus scanner, the indexer)
/// makes `MoveFileExW` fail with `ERROR_ACCESS_DENIED`; then, as std's own
/// rename does, the rename is made with POSIX semantics on a handle of
/// `from` (`SetFileInformationByHandle(FileRenameInfoEx,
/// FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS)`),
/// the handle opened write-through and flushed before it returns, so it is
/// as durable as the first try (#32 F3-r4 5). `Unsupported` off Windows,
/// where `rename(2)` and a directory fsync do the same.
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    match imp::rename(from, to) {
        Err(e) if crate::decide::rename_again(&e) => imp::rename_posix(from, to),
        done => done,
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::path::Path;

    pub(super) fn rename(_from: &Path, _to: &Path) -> io::Result<()> {
        crate::unsupported()
    }

    pub(super) fn rename_posix(_from: &Path, _to: &Path) -> io::Result<()> {
        crate::unsupported()
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::mem::offset_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_WRITE_THROUGH,
        FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileRenameInfoEx, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        SetFileInformationByHandle,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        FILE_RENAME_FLAG_POSIX_SEMANTICS, FILE_RENAME_FLAG_REPLACE_IF_EXISTS,
    };

    /// `p` as NUL-terminated UTF-16.
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
    /// MOVEFILE_WRITE_THROUGH`.
    pub(super) fn rename(from: &Path, to: &Path) -> io::Result<()> {
        let (wide_from, wide_to) = (wide(from), wide(to));
        // SAFETY: two NUL-terminated UTF-16 paths that outlive the call.
        let ok = unsafe {
            MoveFileExW(
                wide_from.as_ptr(),
                wide_to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The rename on a handle of `from` with POSIX semantics, which
    /// replaces a target another process holds with delete sharing (std's
    /// own fallback). The handle is opened write-through, as `MoveFileExW`
    /// opens its own for `MOVEFILE_WRITE_THROUGH`, and with write access,
    /// so the renamed file is flushed (`FlushFileBuffers`) before this
    /// returns: never a plain rename that is not yet on the disk.
    pub(super) fn rename_posix(from: &Path, to: &Path) -> io::Result<()> {
        let file: File = OpenOptions::new()
            .access_mode(DELETE | GENERIC_WRITE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(
                FILE_FLAG_WRITE_THROUGH | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
            .open(from)?;
        // The new name, absolute and without its NUL (FileNameLength counts
        // bytes); the zeroed buffer below ends it with one.
        let name: Vec<u16> = std::path::absolute(to)?.as_os_str().encode_wide().collect();
        let name_bytes = name.len() * 2;
        let too_long = |_| io::Error::other("the new name is too long");
        let length = u32::try_from(name_bytes).map_err(too_long)?;
        let size = offset_of!(FILE_RENAME_INFO, FileName) + name_bytes + 2;
        let size_u32 = u32::try_from(size).map_err(too_long)?;
        // u64 words: the buffer is aligned for FILE_RENAME_INFO (its HANDLE).
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `buffer` is zeroed, aligned for FILE_RENAME_INFO and holds
        // `size` bytes: the header (RootDirectory stays null) and the name
        // with its NUL, copied right after the header's fields.
        unsafe {
            (&raw mut (*info).Anonymous).write(FILE_RENAME_INFO_0 {
                Flags: FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS,
            });
            (&raw mut (*info).FileNameLength).write(length);
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                name.len(),
            );
        }
        // SAFETY: an open handle of `file`, and `info` points at `size`
        // initialised bytes that outlive the call.
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfoEx,
                info.cast::<c_void>().cast_const(),
                size_u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        file.sync_all()
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
