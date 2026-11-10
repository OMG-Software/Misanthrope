//! Native moves for core-owned private journal rotation.
//!
//! These path operations grant no authority and are not workspace file tools.

use std::io;
use std::path::Path;

/// Move a private journal without replacing an existing destination.
///
/// The caller must authorize both paths and their parent directories as trusted,
/// core-owned private storage. It must close the journal writer and hold the
/// owning `ControlStore` lock throughout the operation and reconciliation.
/// This helper does not validate path authority or prevent parent-path races.
///
/// Linux uses `renameat2` with `RENAME_NOREPLACE`. Windows uses `MoveFileExW`
/// without replacement or cross-volume copy flags. Each implementation makes
/// one native move attempt. There is no copy/delete or weaker rename fallback.
/// Success does not establish power-loss durability of directory entries.
///
/// # Errors
///
/// An existing destination returns `AlreadyExists` without replacing it. Native
/// failures propagate, including unsupported flags or cross-volume moves. Other
/// platforms return `Unsupported`. Windows rejects paths with an interior NUL or
/// more than 32,766 UTF-16 code units as `InvalidInput`.
///
/// A non-collision error can require the caller to reconcile the source and
/// destination before it retries or reopens the writer. Do not infer rollback
/// from an error alone.
pub fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    rename_noreplace_impl(source, destination)
}

#[cfg(target_os = "linux")]
fn rename_noreplace_impl(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_noreplace_impl(source: &Path, destination: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let source = terminated_path(source)?;
    let destination = terminated_path(destination)?;
    // SAFETY: Both vectors contain checked, NUL-terminated UTF-16 paths and
    // remain alive and immutable for this synchronous call. MoveFileExW reads
    // but does not retain or take ownership of either pointer. Zero flags forbid
    // replacement, copy/delete fallback, and deferred execution. The safe Rust
    // filesystem API does not expose this Windows no-replace move operation.
    let result = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn terminated_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    // Reserve one unit for the terminator within the native maximum. Bound
    // collection before allocation even if the caller supplies a longer path.
    const MAX_PATH_UNITS: usize = 32_767;
    let mut units: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .take(MAX_PATH_UNITS)
        .collect();
    if units.len() == MAX_PATH_UNITS || units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal path is too long or contains a NUL",
        ));
    }
    units.push(0);
    Ok(units)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn rename_noreplace_impl(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no-replace journal moves are unavailable on this platform",
    ))
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::rename_noreplace;
    use std::fs;
    use std::io;

    #[test]
    fn move_preserves_journal_bytes_and_removes_source() {
        let directory = tempfile::tempdir().expect("private journal directory");
        let source = directory.path().join("active.jsonl");
        let destination = directory.path().join("archive.jsonl");
        let bytes = b"first journal record\nsecond journal record\n";
        fs::write(&source, bytes).expect("write source journal");

        rename_noreplace(&source, &destination).expect("move private journal");

        assert_eq!(fs::read(&destination).expect("read moved journal"), bytes);
        assert_eq!(
            fs::metadata(&source)
                .expect_err("source name was removed")
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn destination_collision_preserves_both_journals() {
        let directory = tempfile::tempdir().expect("private journal directory");
        let source = directory.path().join("active.jsonl");
        let destination = directory.path().join("archive.jsonl");
        fs::write(&source, b"active bytes").expect("write source journal");
        fs::write(&destination, b"archive bytes").expect("write destination journal");

        let error = rename_noreplace(&source, &destination).expect_err("destination exists");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&source).expect("source remains"), b"active bytes");
        assert_eq!(
            fs::read(&destination).expect("destination remains"),
            b"archive bytes"
        );
    }

    #[cfg(windows)]
    #[test]
    fn invalid_windows_path_leaves_source_unchanged() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use std::path::PathBuf;

        let directory = tempfile::tempdir().expect("private journal directory");
        let source = directory.path().join("active.jsonl");
        let destination = directory.path().join("archive.jsonl");
        fs::write(&source, b"active bytes").expect("write source journal");
        let invalid_paths = [
            PathBuf::from(OsString::from_wide(&[97, 0, 98])),
            PathBuf::from(OsString::from_wide(&vec![97; 32_767])),
        ];

        for invalid_path in invalid_paths {
            let error = rename_noreplace(&source, &invalid_path)
                .expect_err("invalid destination is rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            let error = rename_noreplace(&invalid_path, &destination)
                .expect_err("invalid source is rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(fs::read(&source).expect("source remains"), b"active bytes");
            assert_eq!(
                fs::metadata(&destination)
                    .expect_err("destination was not created")
                    .kind(),
                io::ErrorKind::NotFound
            );
        }
    }
}
