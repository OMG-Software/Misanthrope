//! Configuration-time records for workspace directory identity.
//!
//! These records describe a selected directory. They are not access grants and
//! do not prove containment at the time a file is opened.

#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::session::NativePath;

const MAX_NATIVE_PATH_UNITS: usize = 32_768;
const MAX_ANCESTOR_DEPTH: usize = 256;

/// Persisted metadata for one selected workspace directory.
///
/// The path and identity are descriptive configuration. Callers must recheck
/// live identity at use time; this record does not authorize filesystem access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryRecord {
    /// Canonical path encoded in native units for the current host.
    pub(crate) path: NativePath,
    /// Volume or filesystem identifier returned by the platform layer.
    pub(crate) volume: u64,
    /// Stable 128-bit file identifier returned by the platform layer.
    pub(crate) file_id: [u8; 16],
    /// Whether the creation timestamp is before the Unix epoch.
    pub(crate) creation_pre_epoch: bool,
    /// Absolute creation-time seconds from the Unix epoch.
    pub(crate) creation_seconds: u64,
    /// Sub-second creation-time nanoseconds.
    pub(crate) creation_nanos: u32,
}

impl DirectoryRecord {
    /// Capture canonical path, live directory identity, and creation time.
    ///
    /// This is for a trusted configuration selection. The result is not an
    /// access grant and must not replace checks on the object later opened.
    pub(crate) fn capture(path: &Path) -> io::Result<Self> {
        Self::capture_with_created(path, |metadata| metadata.created())
    }

    /// Capture a directory and preserve the original creation-time error.
    ///
    /// The reader is injected so tests can verify that unsupported creation
    /// metadata fails closed. This function has no modification-time fallback.
    fn capture_with_created(
        path: &Path,
        created: impl FnOnce(&fs::Metadata) -> io::Result<SystemTime>,
    ) -> io::Result<Self> {
        let canonical_path = fs::canonicalize(path)?;
        if !canonical_path.is_absolute() {
            return Err(invalid_input("canonical workspace path is not absolute"));
        }
        let file = open_directory(&canonical_path)?;
        let metadata = file.metadata()?;
        validate_directory_metadata(&metadata)?;
        let identity = misanthrope_platform::directory_identity(&file)?;
        let (creation_pre_epoch, creation_seconds, creation_nanos) =
            encode_system_time(created(&metadata)?);
        let path = NativePath::from_path(&canonical_path)
            .map_err(|_| invalid_input("workspace path cannot be encoded natively"))?;
        let record = Self {
            path,
            volume: identity.volume,
            file_id: identity.file_id,
            creation_pre_epoch,
            creation_seconds,
            creation_nanos,
        };
        if !record.validate_shape() {
            return Err(invalid_input("workspace directory record is invalid"));
        }
        Ok(record)
    }

    /// Check persisted fields without touching the filesystem.
    ///
    /// Foreign native encodings are structurally valid for portable replay, but
    /// only the current host can convert one into a live path.
    pub(crate) fn validate_shape(&self) -> bool {
        if self.creation_nanos >= 1_000_000_000 {
            return false;
        }
        let units_valid = match &self.path {
            NativePath::UnixBytes(units) => {
                !units.is_empty() && units.len() <= MAX_NATIVE_PATH_UNITS && !units.contains(&0)
            }
            NativePath::WindowsUtf16(units) => {
                !units.is_empty() && units.len() <= MAX_NATIVE_PATH_UNITS && !units.contains(&0)
            }
        };
        if !units_valid {
            return false;
        }

        #[cfg(unix)]
        if let NativePath::UnixBytes(units) = &self.path {
            return units.first() == Some(&b'/');
        }
        #[cfg(windows)]
        if let NativePath::WindowsUtf16(units) = &self.path {
            use std::os::windows::ffi::OsStringExt;
            return PathBuf::from(std::ffi::OsString::from_wide(units)).is_absolute();
        }
        true
    }

    /// Decode a native path for this host and require an absolute path.
    pub(crate) fn live_path(&self) -> io::Result<PathBuf> {
        if !self.validate_shape() {
            return Err(invalid_input("workspace directory record is invalid"));
        }
        let path = self
            .path
            .to_path_buf()
            .map_err(|_| invalid_input("workspace path encoding does not match this host"))?;
        if !path.is_absolute() {
            return Err(invalid_input("workspace path is not absolute"));
        }
        Ok(path)
    }

    /// Re-capture the recorded path and compare its live identity and creation time.
    pub(crate) fn is_current(&self) -> io::Result<bool> {
        let path = self.live_path()?;
        match Self::capture(&path) {
            Ok(current) => Ok(self.same_directory(&current)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Check whether `other` is this directory or one of its descendants.
    ///
    /// The method walks live directory handles by identity. It uses no string
    /// prefix check. The result is a configuration-time observation, not proof
    /// that later file access will remain contained.
    pub(crate) fn contains(&self, other: &Self) -> io::Result<bool> {
        if !self.is_current()? || !other.is_current()? {
            return Ok(false);
        }
        let self_identity = (self.volume, self.file_id);
        let mut ancestor = other.live_path()?;

        for _ in 0..MAX_ANCESTOR_DEPTH {
            let file = open_directory(&ancestor)?;
            let metadata = file.metadata()?;
            validate_directory_metadata(&metadata)?;
            let identity = misanthrope_platform::directory_identity(&file)?;
            if (identity.volume, identity.file_id) == self_identity {
                return Ok(true);
            }
            let Some(parent) = ancestor.parent() else {
                return Ok(false);
            };
            if parent == ancestor {
                return Ok(false);
            }
            ancestor = parent.to_path_buf();
        }
        Err(invalid_input("directory ancestor depth limit exceeded"))
    }

    /// Compare directory identity and creation time, not mutable child metadata.
    pub(crate) fn same_directory(&self, other: &Self) -> bool {
        self.volume == other.volume
            && self.file_id == other.file_id
            && self.creation_pre_epoch == other.creation_pre_epoch
            && self.creation_seconds == other.creation_seconds
            && self.creation_nanos == other.creation_nanos
    }
}

/// Store a time as a before-epoch flag plus the exact duration magnitude.
///
/// This preserves the `SystemTime` value reported by the host metadata API.
fn encode_system_time(value: SystemTime) -> (bool, u64, u32) {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => (false, duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            (true, duration.as_secs(), duration.subsec_nanos())
        }
    }
}

/// Open a directory without following its final link or blocking on special files.
fn open_directory(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // Win32 FILE_FLAG_BACKUP_SEMANTICS permits directory handles. Opening
        // the final reparse point itself lets the metadata check reject it.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory handles are unsupported on this platform",
        ))
    }
}

/// Require an ordinary directory and reject a Windows reparse-point handle.
///
/// The check describes the opened object. It does not secure its parent path.
fn validate_directory_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_dir() {
        return Err(invalid_input("workspace path is not a directory"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_input("workspace directory is a reparse point"));
        }
    }
    Ok(())
}

/// Build a consistent error for invalid directory metadata or path shape.
fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn capture(path: &Path) -> DirectoryRecord {
        DirectoryRecord::capture(path).expect("directory record captured")
    }

    #[test]
    fn captured_record_round_trips_through_json() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let record = capture(directory.path());
        let encoded = serde_json::to_vec(&record).expect("record serializes");
        let decoded: DirectoryRecord =
            serde_json::from_slice(&encoded).expect("record deserializes");

        assert_eq!(decoded, record);
        assert!(decoded.validate_shape());
        assert_eq!(
            decoded.live_path().expect("native path decodes"),
            record.live_path().expect("path decodes")
        );
    }

    #[test]
    fn regular_files_are_rejected_as_workspace_directories() {
        let file = tempfile::NamedTempFile::new().expect("temporary file");

        let error = DirectoryRecord::capture(file.path()).expect_err("file is not a directory");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory
        ));
    }

    #[test]
    fn unavailable_creation_time_propagates_without_fallback() {
        let directory = tempfile::tempdir().expect("temporary directory");

        let error = DirectoryRecord::capture_with_created(directory.path(), |_| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "directory creation time unavailable",
            ))
        })
        .expect_err("capture must fail when creation time is unavailable");

        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), "directory creation time unavailable");
    }

    #[test]
    fn child_writes_do_not_change_directory_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let record = capture(directory.path());

        fs::write(directory.path().join("child.txt"), b"content").expect("write child file");

        assert!(record.is_current().expect("identity check succeeds"));
    }

    #[test]
    fn containment_accepts_descendants_and_rejects_siblings() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("root");
        let child = root.join("child");
        let sibling = directory.path().join("sibling");
        fs::create_dir_all(&child).expect("create child directory");
        fs::create_dir(&sibling).expect("create sibling directory");
        let root_record = capture(&root);
        let child_record = capture(&child);
        let sibling_record = capture(&sibling);

        assert!(
            root_record
                .contains(&child_record)
                .expect("containment walk succeeds")
        );
        assert!(
            !root_record
                .contains(&sibling_record)
                .expect("sibling walk succeeds")
        );
        assert!(
            root_record
                .contains(&root_record)
                .expect("self contains itself")
        );
    }

    #[test]
    fn replacement_at_the_same_path_is_not_current() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let selected = directory.path().join("workspace");
        let moved = directory.path().join("workspace-old");
        fs::create_dir(&selected).expect("create selected directory");
        let record = capture(&selected);
        fs::rename(&selected, &moved).expect("move original directory");
        fs::create_dir(&selected).expect("replace original path");

        assert!(
            !record
                .is_current()
                .expect("replacement identity check succeeds")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_captures_the_canonical_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("target");
        let alias = directory.path().join("alias");
        fs::create_dir(&target).expect("create target");
        symlink(&target, &alias).expect("create symlink alias");

        let target_record = capture(&target);
        let alias_record = capture(&alias);
        assert!(target_record.same_directory(&alias_record));
        assert_eq!(
            alias_record.live_path().expect("canonical path"),
            target_record.live_path().expect("target path")
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_unix_path_round_trips_losslessly() {
        use std::os::unix::ffi::OsStringExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let native_name = std::ffi::OsString::from_vec(vec![b'w', b's', 0xff]);
        let path = directory.path().join(native_name);
        fs::create_dir(&path).expect("create directory with non-Unicode name");
        let record = capture(&path);

        assert!(matches!(record.path, NativePath::UnixBytes(_)));
        assert_eq!(
            record.live_path().expect("native path preserved"),
            fs::canonicalize(path).expect("canonical path")
        );
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_windows_path_round_trips_losslessly() {
        use std::os::windows::ffi::OsStrExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let mut units: Vec<u16> = directory.path().as_os_str().encode_wide().collect();
        units.extend([u16::from(b'\\'), 0xd800]);
        let native = NativePath::WindowsUtf16(units.clone());
        let decoded = native.to_path_buf().expect("UTF-16 path decodes");

        assert!(decoded.is_absolute());
        assert_eq!(decoded.as_os_str().encode_wide().collect::<Vec<_>>(), units);
        assert!(
            DirectoryRecord {
                path: native,
                volume: 1,
                file_id: [1; 16],
                creation_pre_epoch: false,
                creation_seconds: 0,
                creation_nanos: 0,
            }
            .validate_shape()
        );
    }
    #[test]
    fn validate_shape_rejects_invalid_time_and_unbounded_paths() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut record = capture(directory.path());
        record.creation_nanos = 1_000_000_000;
        assert!(!record.validate_shape());

        let mut record = capture(directory.path());
        #[cfg(unix)]
        {
            record.path = NativePath::UnixBytes(vec![b'/'; MAX_NATIVE_PATH_UNITS + 1]);
        }
        #[cfg(windows)]
        {
            let mut units = vec![u16::from(b'C'), u16::from(b':'), u16::from(b'\\')];
            units.resize(MAX_NATIVE_PATH_UNITS + 1, u16::from(b'x'));
            record.path = NativePath::WindowsUtf16(units);
        }
        assert!(!record.validate_shape());
    }

    #[test]
    fn creation_time_codec_preserves_pre_epoch_and_nanoseconds() {
        let value = UNIX_EPOCH - Duration::new(17, 123_456_700);

        let (pre_epoch, seconds, nanos) = encode_system_time(value);

        assert!(pre_epoch);
        assert_eq!(seconds, 17);
        assert_eq!(nanos, 123_456_700);
    }
}
