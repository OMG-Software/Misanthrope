//! Shared private persistence primitives for core-owned journals.
//!
//! These helpers validate opened files and provide single-writer locking. They
//! do not authorize paths or contain access to untrusted parent directories.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use thiserror::Error;

/// A failure while creating, opening, or syncing private storage.
#[derive(Debug, Error)]
pub(crate) enum StorageError {
    /// An operating-system operation failed. The original cause is retained.
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The opened object does not meet the expected regular-file contract.
    #[error("invalid storage object: {0}")]
    Invalid(&'static str),
    /// Another process or handle owns the exclusive writer lock.
    #[error("storage object is already open for writing")]
    Busy,
}

/// Create missing private directory components. Existing ACLs are not changed.
///
/// Unix requests mode 0700 for each created directory. Windows uses the
/// inherited directory ACL. `path` and all parent directories must be trusted;
/// this helper is not a path authorization or containment boundary.
pub(crate) fn create_private_directories(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Open and exclusively lock a regular, single-link file without following
/// links or permitting replacement while the returned handle remains alive.
///
/// `create_new` creates a new file and requests Unix mode 0600. Otherwise the
/// existing file opens read/write. The caller owns the returned handle and must
/// retain it for the required lock lifetime. Parent directories must be trusted;
/// this helper does not authorize paths or prevent parent-directory races.
pub(crate) fn open_locked_file(path: &Path, create_new: bool) -> Result<File, StorageError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create_new);
    configure_open_options(&mut options);
    #[cfg(unix)]
    if create_new {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    validate_open_file(path, &file)?;
    file.try_lock().map_err(map_lock_error)?;
    validate_opened_file(&file)?;
    Ok(file)
}

/// Sync the containing directory entry on Unix. Rust does not provide a
/// qualified safe directory-handle sync contract on Windows, so this is a
/// documented no-op there. The caller chooses the parent directory to sync.
pub(crate) fn sync_parent_directory(directory: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(windows)]
    {
        let _ = directory;
    }
    Ok(())
}

/// Parse exactly one JSON value and reject duplicate object keys at every depth.
pub(crate) fn parse_strict_json(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value.0)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Number::from_f64(value)
                    .map(|number| StrictValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::String(value.to_owned())))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(value)))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }

            /// Keep duplicate-key checks active inside nested array elements.
            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }

            /// Detect repeated keys before converting an object to a JSON map.
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = Map::new();
                while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    values.insert(key, value.0);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Configure handle sharing on Windows so a live journal cannot be replaced.
fn configure_open_options(options: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ_AND_WRITE: u32 = 0x0000_0003;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ_AND_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(windows))]
    {
        let _ = options;
    }
}

/// Validate the handle and confirm Unix path identity before acquiring its lock.
fn validate_open_file(path: &Path, file: &File) -> Result<(), StorageError> {
    validate_opened_file(file)?;
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(StorageError::Invalid(
            "file path is a link or not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if opened.dev() != path_metadata.dev() || opened.ino() != path_metadata.ino() {
            return Err(StorageError::Invalid("file path changed during open"));
        }
    }
    Ok(())
}

/// Recheck the opened object after locking, when link changes can be detected.
fn validate_opened_file(file: &File) -> Result<(), StorageError> {
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(StorageError::Invalid("file handle is not a regular file"));
    }
    match misanthrope_platform::validate_journal_handle(file) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            return Err(StorageError::Invalid(
                "file handle is linked or not a regular file",
            ));
        }
        Err(error) => return Err(StorageError::Io(error)),
    }
    Ok(())
}

/// Distinguish an occupied lock from a lock-facility error.
fn map_lock_error(error: std::fs::TryLockError) -> StorageError {
    match error {
        std::fs::TryLockError::WouldBlock => StorageError::Busy,
        std::fs::TryLockError::Error(error) => StorageError::Io(error),
    }
}
