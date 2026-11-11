//! Native, handle-relative workspace I/O. This is not process containment.
//!
//! Callers must separately authorize operations and verify creation times.
//! Linux and Windows provide a native replacement boundary through retained handles;
//! ordinary create operations never replace existing files. Concurrent
//! hostile same-user mutation of filesystem namespaces remains outside this boundary.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::DirectoryIdentity;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", windows))]
mod replacement;
#[cfg(windows)]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
use linux::NativeWorkspace;
#[cfg(windows)]
use windows::NativeWorkspace;

#[cfg(any(target_os = "linux", windows))]
pub use replacement::{ReplacementStage, ReplacementTarget};

/// Maximum bytes read or supplied to a single new-file operation.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Maximum directory entries returned by one listing.
pub const MAX_DIRECTORY_ENTRIES: usize = 512;
/// Maximum aggregate native name units returned by one listing.
pub const MAX_DIRECTORY_NAME_UNITS: usize = 64 * 1024;
const MAX_PATH_UNITS: usize = 4096;
const MAX_COMPONENT_UNITS: usize = 255;
const MAX_COMPONENTS: usize = 32;

/// A bounded sequence of native names. It grants no filesystem authority.
///
/// Empty paths identify the workspace for listing only. Other paths reject
/// roots, prefixes, empty components, dot components, NULs and traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativePath {
    components: Vec<OsString>,
}

impl RelativePath {
    /// Validate a native relative path without resolving filesystem objects.
    pub fn new(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let units = path.as_os_str().as_bytes();
            validate_length(units.len())?;
            if units.is_empty() {
                return Ok(Self {
                    components: Vec::new(),
                });
            }
            let mut components = Vec::new();
            for name in units.split(|unit| *unit == b'/') {
                validate_component(
                    name.len(),
                    name == b"." || name == b".." || name.contains(&0),
                )?;
                components.push(OsString::from_vec(name.to_vec()));
                validate_depth(components.len())?;
            }
            Ok(Self { components })
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let units: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .take(MAX_PATH_UNITS + 1)
                .collect();
            validate_length(units.len())?;
            if units.is_empty() {
                return Ok(Self {
                    components: Vec::new(),
                });
            }
            let mut components = Vec::new();
            for name in units.split(|unit| *unit == u16::from(b'\\') || *unit == u16::from(b'/')) {
                validate_component(name.len(), invalid_windows_name(name))?;
                components.push(OsString::from_wide(name));
                validate_depth(components.len())?;
            }
            Ok(Self { components })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    /// Whether this path denotes the workspace directory itself.
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// Reconstruct the validated native names without Unicode replacement.
    pub fn to_path_buf(&self) -> PathBuf {
        self.components.iter().collect()
    }
}

/// Reject names whose exact byte/code-unit representation exceeds our bounds.
fn validate_length(length: usize) -> io::Result<()> {
    if length > MAX_PATH_UNITS {
        return Err(invalid("relative path exceeds 4096 native units"));
    }
    Ok(())
}

/// Reject normalization-sensitive or ambiguous components before any native open.
fn validate_component(length: usize, invalid_name: bool) -> io::Result<()> {
    if length == 0 || length > MAX_COMPONENT_UNITS || invalid_name {
        return Err(invalid("invalid relative path component"));
    }
    Ok(())
}

/// Keep native traversal work bounded independently of the encoded path size.
fn validate_depth(depth: usize) -> io::Result<()> {
    if depth > MAX_COMPONENTS {
        return Err(invalid("relative path exceeds 32 components"));
    }
    Ok(())
}

#[cfg(windows)]
/// Refuse DOS aliases, alternate streams and Win32-normalized spellings.
pub(crate) fn invalid_windows_name(name: &[u16]) -> bool {
    if name.is_empty() || name == [46] || name == [46, 46] || matches!(name.last(), Some(32 | 46)) {
        return true;
    }
    if name
        .iter()
        .any(|unit| *unit < 32 || [34, 42, 58, 60, 62, 63, 124].contains(unit))
    {
        return true;
    }
    let stem: Vec<u16> = name
        .iter()
        .copied()
        .take_while(|unit| *unit != 46)
        .map(|unit| {
            if (97..=122).contains(&unit) {
                unit - 32
            } else {
                unit
            }
        })
        .collect();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| reserved.encode_utf16().eq(stem.iter().copied()))
    {
        return true;
    }
    stem.len() == 4
        && (stem.starts_with(&[67, 79, 77]) || stem.starts_with(&[76, 80, 84]))
        && stem
            .last()
            .is_some_and(|unit| (49..=57).contains(unit) || [0x00b9, 0x00b2, 0x00b3].contains(unit))
}

/// Descriptive kind only. Every later access must reopen and validate its object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Blocked,
}

/// A native entry name from an opened directory. Names are untrusted data.
#[derive(Debug)]
pub struct DirectoryEntry {
    /// Native name; never use lossy display text to reopen it.
    pub name: OsString,
    /// Observed kind. Blocked entries cannot be read through this broker.
    pub kind: EntryKind,
}

/// A bounded observation, not a consistent directory snapshot or access grant.
#[derive(Debug)]
pub struct Listing {
    /// At most the requested number of native names.
    pub entries: Vec<DirectoryEntry>,
    /// Additional entries or name units exceeded the requested bounds.
    pub truncated: bool,
}

/// A creation failure that distinguishes uncertain mutation from refusal.
#[derive(Debug)]
pub struct CreateError {
    /// Original operating-system failure.
    pub error: io::Error,
    /// A new entry may remain. It must not be automatically retried or deleted.
    pub may_have_created: bool,
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(output)
    }
}
impl std::error::Error for CreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Captured source bytes and native metadata from the same retained file.
/// This is evidence, not a write grant or an atomic filesystem snapshot.
#[derive(Debug)]
pub struct FileCapture {
    /// Exact bounded bytes read from the pinned regular single-link file.
    pub bytes: Vec<u8>,
    /// Native volume identifier.
    pub volume: u64,
    /// Native file ID in the existing 16-byte representation.
    pub file_id: [u8; 16],
    /// Native security from that file. Unsupported metadata is refused.
    pub security: crate::private_store::NativeSecurity,
}

/// An anchored workspace. Policy and session checks belong to the trusted core.
pub struct Workspace {
    native: NativeWorkspace,
}

impl Workspace {
    /// Open the selected root and workspace and compare the actual directory IDs.
    /// Unsupported native controls fail closed; there is no ambient-path fallback.
    pub fn open(
        root: &Path,
        expected_root: DirectoryIdentity,
        workspace_relative: &RelativePath,
        expected_workspace: DirectoryIdentity,
    ) -> io::Result<Self> {
        Ok(Self {
            native: NativeWorkspace::open(
                root,
                expected_root,
                workspace_relative,
                expected_workspace,
            )?,
        })
    }

    /// Creation times from the retained directory handles, for core identity checks.
    pub fn creation_times(&self) -> io::Result<(SystemTime, SystemTime)> {
        self.native.creation_times()
    }

    /// Read one ordinary single-link file, bounded during the read itself.
    pub fn read(&self, path: &RelativePath, max_bytes: usize) -> io::Result<Vec<u8>> {
        require_file_target(path)?;
        if max_bytes == 0 || max_bytes > MAX_FILE_BYTES {
            return Err(invalid("invalid file read limit"));
        }
        self.native.read(path, max_bytes)
    }

    /// Capture checkpoint source bytes, identity, and supported security metadata.
    /// Linux refuses extended attributes and special permission bits. Other
    /// backends refuse this operation until their metadata contract is qualified.
    pub fn capture(&self, path: &RelativePath, max_bytes: usize) -> io::Result<FileCapture> {
        require_file_target(path)?;
        if max_bytes == 0 || max_bytes > MAX_FILE_BYTES {
            return Err(invalid("invalid checkpoint capture limit"));
        }
        #[cfg(any(target_os = "linux", windows))]
        {
            self.native.capture(path, max_bytes)
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "checkpoint source capture is unavailable",
            ))
        }
    }

    /// Prepare a retained target for the native replacement protocol.
    /// This does not authorize publication; callers must check live authority.
    #[cfg(any(target_os = "linux", windows))]
    pub fn replacement_target(
        &self,
        path: &RelativePath,
        max_bytes: usize,
    ) -> io::Result<ReplacementTarget<'_>> {
        require_file_target(path)?;
        if max_bytes == 0 || max_bytes > MAX_FILE_BYTES {
            return Err(invalid("invalid replacement capture limit"));
        }
        self.native.replacement_target(path, max_bytes)
    }

    /// Enumerate native names through a directory handle, with bounded output.
    pub fn list(
        &self,
        path: &RelativePath,
        max_entries: usize,
        max_name_units: usize,
    ) -> io::Result<Listing> {
        if !(1..=MAX_DIRECTORY_ENTRIES).contains(&max_entries)
            || !(1..=MAX_DIRECTORY_NAME_UNITS).contains(&max_name_units)
        {
            return Err(invalid("invalid directory listing limits"));
        }
        self.native.list(path, max_entries, max_name_units)
    }

    /// Exclusively create a file. Partial creation is reported without deleting it.
    pub fn create_new(&self, path: &RelativePath, data: &[u8]) -> Result<(), CreateError> {
        require_file_target(path).map_err(not_created)?;
        if data.len() > MAX_FILE_BYTES {
            return Err(not_created(invalid("new file exceeds 1 MiB")));
        }
        self.native.create_new(path, data)
    }

    /// Exclusively create one directory beneath existing directories.
    pub fn create_dir(&self, path: &RelativePath) -> Result<(), CreateError> {
        require_file_target(path).map_err(not_created)?;
        self.native.create_dir(path)
    }
}

/// A workspace root marker is valid only for directory listing.
fn require_file_target(path: &RelativePath) -> io::Result<()> {
    if path.is_empty() {
        return Err(invalid("operation requires a relative entry name"));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn not_created(error: io::Error) -> CreateError {
    CreateError {
        error,
        may_have_created: false,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native workspace broker is unavailable",
    )
}

#[cfg(not(any(target_os = "linux", windows)))]
struct NativeWorkspace;
#[cfg(not(any(target_os = "linux", windows)))]
impl NativeWorkspace {
    fn open(
        _: &Path,
        _: DirectoryIdentity,
        _: &RelativePath,
        _: DirectoryIdentity,
    ) -> io::Result<Self> {
        Err(unsupported())
    }
    fn creation_times(&self) -> io::Result<(SystemTime, SystemTime)> {
        Err(unsupported())
    }
    fn read(&self, _: &RelativePath, _: usize) -> io::Result<Vec<u8>> {
        Err(unsupported())
    }
    fn list(&self, _: &RelativePath, _: usize, _: usize) -> io::Result<Listing> {
        Err(unsupported())
    }
    fn create_new(&self, _: &RelativePath, _: &[u8]) -> Result<(), CreateError> {
        Err(not_created(unsupported()))
    }
    fn create_dir(&self, _: &RelativePath) -> Result<(), CreateError> {
        Err(not_created(unsupported()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_refuse_traversal_and_ambiguous_components() {
        for name in [
            "/outside", "..", "a/../b", "./a", "a/./b", "a//b", "a/", "a\0b",
        ] {
            assert!(RelativePath::new(Path::new(name)).is_err(), "{name:?}");
        }
        assert!(
            RelativePath::new(Path::new(""))
                .expect("root marker")
                .is_empty()
        );
        assert_eq!(
            RelativePath::new(Path::new("src/main.rs"))
                .expect("relative names")
                .to_path_buf(),
            Path::new("src/main.rs")
        );
    }

    #[test]
    fn path_work_and_allocation_have_explicit_bounds() {
        assert!(RelativePath::new(Path::new(&"x".repeat(256))).is_err());
        assert!(RelativePath::new(Path::new(&vec!["x"; 33].join("/"))).is_err());
        assert!(RelativePath::new(Path::new(&"x".repeat(4097))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_refuse_namespaces_streams_and_device_aliases() {
        for name in [
            "C:\\outside",
            "\\\\server\\share",
            "a:b",
            "NUL",
            "con.txt",
            "COM1",
            "LPT9.txt",
            "a.",
            "a ",
            "a*",
            "a?",
        ] {
            assert!(RelativePath::new(Path::new(name)).is_err(), "{name:?}");
        }
    }
}
