//! Native handle validation. This does not implement an execution sandbox.

use std::fs::File;
use std::io;

/// Reject non-regular, reparse-point, or multiply linked journal handles.
///
/// The caller retains ownership of the file. This inspects the already opened
/// object; it does not authorize a path or make parent directories trustworthy.
pub fn validate_journal_handle(file: &File) -> io::Result<()> {
    validate(file)
}

#[cfg(unix)]
fn validate(file: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal must be a regular file with one hard link",
        ));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn validate(file: &File) -> io::Result<()> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `file` owns a live handle throughout this synchronous call. The
    // output points to aligned writable storage of the exact Windows ABI type.
    // Success initializes the structure; failure returns before any field read.
    // The API borrows the handle and does not close or transfer its ownership.
    let information = unsafe {
        if GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) == 0 {
            return Err(io::Error::last_os_error());
        }
        information.assume_init()
    };
    if information.nNumberOfLinks != 1
        || information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal must be a regular non-reparse file with one hard link",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native journal handle validation is unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::validate_journal_handle;
    use std::fs;
    use std::io::Write;

    #[test]
    fn regular_file_is_accepted_and_ownership_stays_with_caller() {
        let mut file = tempfile::NamedTempFile::new().expect("private temporary file");
        validate_journal_handle(file.as_file()).expect("regular single-link file");
        file.write_all(b"still owned").expect("handle remains open");
    }

    #[test]
    fn multiple_hard_links_are_rejected_from_the_open_handle() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let source = directory.path().join("source");
        let alias = directory.path().join("alias");
        let file = fs::File::create(&source).expect("source");
        fs::hard_link(&source, &alias).expect("hard link on supported test filesystem");
        assert!(validate_journal_handle(&file).is_err());
    }
}
