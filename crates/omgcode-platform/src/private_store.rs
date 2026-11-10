//! Handle-relative private storage. This is not workspace authorization.
//!
//! Linux requires openat2 and procfs. Other backends refuse before I/O until
//! private ACL and directory persistence requirements have been qualified.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::PrivateDirectory;

/// Native identity metadata for one already-open private regular file.
/// These values do not authorize access or prove checkpoint durability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateFileIdentity {
    /// Native volume identifier.
    pub volume: u64,
    /// Native file identifier in the existing 16-byte representation.
    pub file_id: [u8; 16],
}

/// Inspect the same private file handle whose bytes the caller reads or hashes.
/// Linux requires a regular, single-link, current-user-owned mode0600 file.
/// Other platforms refuse before I/O. No pathname lookup or reopen occurs.
pub fn file_identity(file: &std::fs::File) -> std::io::Result<PrivateFileIdentity> {
    #[cfg(target_os = "linux")]
    {
        linux::file_identity(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(unsupported())
    }
}

#[cfg(not(target_os = "linux"))]
use crate::DirectoryIdentity;
#[cfg(not(target_os = "linux"))]
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::SystemTime,
};

/// An unavailable native private-directory backend.
#[cfg(not(target_os = "linux"))]
pub struct PrivateDirectory;

#[cfg(not(target_os = "linux"))]
impl PrivateDirectory {
    /// Refuse unsupported private storage before opening or creating anything.
    pub fn open(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported native identity lookup.
    pub fn identity(&self) -> io::Result<DirectoryIdentity> {
        Err(unsupported())
    }
    /// Refuse unsupported native creation-time lookup.
    pub fn created(&self) -> io::Result<SystemTime> {
        Err(unsupported())
    }
    /// Refuse unsupported relative directory opening.
    pub fn open_dir(&self, _name: &OsStr) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported directory creation.
    pub fn create_dir(&self, _name: &OsStr) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Refuse unsupported private file opening.
    pub fn open_file(&self, _name: &OsStr, _writable: bool) -> io::Result<File> {
        Err(unsupported())
    }
    /// Refuse unsupported private file creation.
    pub fn create_file(&self, _name: &OsStr) -> io::Result<File> {
        Err(unsupported())
    }
    /// Refuse unsupported existing-entry probing.
    pub fn exists(&self, _name: &OsStr) -> io::Result<bool> {
        Err(unsupported())
    }
    /// Refuse unsupported private-directory enumeration.
    pub fn names(&self, _limit: usize) -> io::Result<Vec<OsString>> {
        Err(unsupported())
    }
    /// Refuse unsupported directory synchronization, rather than a no-op.
    pub fn sync(&self) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native private storage is not qualified on this platform",
    )
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    #[test]
    fn unsupported_backend_does_not_create_storage() {
        let root = tempfile::tempdir().expect("fixture");
        let missing = root.path().join("never-created");
        assert_eq!(
            PrivateDirectory::open(&missing)
                .err()
                .expect("unsupported")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!missing.exists());
    }
}
