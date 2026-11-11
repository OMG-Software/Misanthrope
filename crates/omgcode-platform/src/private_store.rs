//! Handle-relative private storage. This is not workspace authorization.
//!
//! Linux requires openat2 and procfs. Windows requires NTFS and verified ACLs.
//! Other backends refuse before I/O.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
pub use linux::PrivateDirectory;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::WindowsPrivateDirectory as PrivateDirectory;

/// Native identity metadata for one already-open private regular file.
/// These values do not authorize access or prove checkpoint durability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrivateFileIdentity {
    /// Native volume identifier.
    pub volume: u64,
    /// Native file identifier in the existing 16-byte representation.
    pub file_id: [u8; 16],
}

/// Native security that a replacement must carry over from its source.
///
/// This is captured evidence. It never authorizes access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSecurity {
    /// Linux ownership and ordinary permission bits.
    Unix {
        /// File owner ID.
        owner: u32,
        /// File group ID.
        group: u32,
        /// Ordinary permission bits only.
        mode: u32,
    },
    /// Windows owner, group, and DACL, plus the content-indexing opt-out.
    Windows {
        /// Canonical SDDL read from one open handle.
        descriptor: String,
        /// Whether the file is excluded from content indexing.
        not_content_indexed: bool,
    },
}

/// Metadata that a replacement stage can preserve from its source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageMetadata {
    /// Identity of the already-open, regular, single-link file.
    pub identity: PrivateFileIdentity,
    /// Security read from that same file.
    pub security: NativeSecurity,
}

/// Inspect the same private file handle whose bytes the caller reads or hashes.
/// Linux requires a regular, single-link, current-user-owned mode0600 file.
/// Windows requires a regular, single-link file with the exact private DACL.
/// Other platforms refuse before I/O. No pathname lookup or reopen occurs.
pub fn file_identity(file: &std::fs::File) -> std::io::Result<PrivateFileIdentity> {
    #[cfg(target_os = "linux")]
    {
        linux::file_identity(file)
    }
    #[cfg(windows)]
    {
        windows::file_identity(file)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = file;
        Err(unsupported())
    }
}

/// Inspect supported replacement-stage metadata from the same open file handle.
/// Linux rejects xattrs, special mode bits, and file attrs. Windows rejects
/// alternate streams, extended attributes, and unsupported file attributes.
pub fn stage_metadata(file: &std::fs::File) -> std::io::Result<StageMetadata> {
    #[cfg(target_os = "linux")]
    {
        linux::stage_metadata(file)
    }
    #[cfg(windows)]
    {
        windows::stage_metadata(file)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = file;
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
use crate::DirectoryIdentity;
#[cfg(not(any(target_os = "linux", windows)))]
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::SystemTime,
};

/// An unavailable native private-directory backend.
#[cfg(not(any(target_os = "linux", windows)))]
pub struct PrivateDirectory;

#[cfg(not(any(target_os = "linux", windows)))]
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
    /// Refuse unsupported stage inspection before file access.
    pub fn open_stage(&self) -> io::Result<File> {
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

#[cfg(not(any(target_os = "linux", windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native private storage is not qualified on this platform",
    )
}

#[cfg(all(test, not(any(target_os = "linux", windows))))]
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
