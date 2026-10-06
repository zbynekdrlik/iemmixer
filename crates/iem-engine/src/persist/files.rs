//! The file operations persistence makes (#32 P9), behind one seam so a
//! test can fail any single step (a write, an fsync, a rename, a directory
//! sync, a removal, a read, a listing) and check that no state rolls back.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The rename the state files go through (#32 minor-7): on Windows
/// `MoveFileExW` with write-through, on the disk when it returns (Windows
/// has no directory handle `sync_dir` could flush); elsewhere `rename(2)`,
/// which `sync_dir` makes durable. A save reported saved survives a power
/// cut either way.
#[cfg(windows)]
use iem_win::file::rename_durable as os_rename;
#[cfg(not(windows))]
use std::fs::rename as os_rename;

/// The pause between two tries of a read that failed with an error that
/// may pass (`chain` bounds how many one load takes).
const READ_PAUSE: Duration = Duration::from_millis(200);

/// A directory's entries (name, path), each or the error reading it.
pub(crate) type Entries = Vec<io::Result<(OsString, PathBuf)>>;

pub(crate) trait Files: fmt::Debug + Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Creates or truncates `path` and writes `bytes` into it (no fsync).
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Flushes the file at `path` to the disk.
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Whether `path` exists; an error when that cannot be told.
    fn exists(&self, path: &Path) -> io::Result<bool>;
    fn list(&self, dir: &Path) -> io::Result<Entries>;
    /// Flushes the directory's entries (the renames) to the disk.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Waits before a read that failed is tried again.
    fn pause(&self);
}

/// The file system itself.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OsFiles;

impl Files for OsFiles {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        File::create(path)?.write_all(bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        // A handle with write access: Windows flushes only through one.
        fs::OpenOptions::new().write(true).open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        os_rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        path.try_exists()
    }

    fn list(&self, dir: &Path) -> io::Result<Entries> {
        Ok(fs::read_dir(dir)?
            .map(|e| e.map(|e| (e.file_name(), e.path())))
            .collect())
    }

    #[cfg(unix)]
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }

    /// Windows has no directory handle to flush this way; each rename is
    /// on the disk when it returns (`os_rename`, write-through).
    #[cfg(not(unix))]
    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    fn pause(&self) {
        std::thread::sleep(READ_PAUSE);
    }
}
