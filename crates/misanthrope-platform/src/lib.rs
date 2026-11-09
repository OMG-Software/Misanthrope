//! Native handle validation. This does not implement an execution sandbox.

pub mod broker;

use std::fs::File;
use std::io;

/// Native volume and file identity read from an open directory handle.
///
/// These values are metadata only. They do not authorize access or establish
/// filesystem containment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryIdentity {
    /// Native volume identifier.
    pub volume: u64,
    /// Native file identifier encoded as 16 bytes.
    pub file_id: [u8; 16],
}

/// Read the native identity of an already-open directory.
///
/// The function borrows `directory`; it does not close or transfer the handle.
/// It rejects handles that do not identify an ordinary directory. The returned
/// identity is descriptive metadata only, not an authorization or sandbox check.
pub fn directory_identity(directory: &File) -> io::Result<DirectoryIdentity> {
    directory_identity_impl(directory)
}

#[cfg(unix)]
fn directory_identity_impl(directory: &File) -> io::Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory identity requires a directory handle",
        ));
    }
    Ok(DirectoryIdentity {
        volume: metadata.dev(),
        file_id: u128::from(metadata.ino()).to_le_bytes(),
    })
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn directory_identity_impl(directory: &File) -> io::Result<DirectoryIdentity> {
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    let handle = directory.as_raw_handle();
    let mut attributes = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `directory` owns a live handle for both synchronous API calls.
    // The output is aligned storage for the exact Win32 structure. Success
    // initializes it; failure returns before any field is read. These APIs
    // borrow the handle and do not close or transfer its ownership.
    let attributes = unsafe {
        if GetFileInformationByHandle(handle, attributes.as_mut_ptr()) == 0 {
            return Err(io::Error::last_os_error());
        }
        attributes.assume_init()
    };
    if attributes.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || attributes.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory identity requires a non-reparse directory handle",
        ));
    }

    let buffer_size = u32::try_from(size_of::<FILE_ID_INFO>()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "native file identity structure is too large",
        )
    })?;
    let mut identity = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: `directory` still owns the live borrowed handle. The pointer is
    // aligned storage for `FILE_ID_INFO`, and `buffer_size` is its checked size.
    // Success initializes that structure; failure returns before field access.
    // The API does not retain or take ownership of the handle or output buffer.
    let identity = unsafe {
        if GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            identity.as_mut_ptr().cast(),
            buffer_size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        identity.assume_init()
    };
    Ok(DirectoryIdentity {
        volume: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
    })
}

#[cfg(not(any(unix, windows)))]
fn directory_identity_impl(_directory: &File) -> io::Result<DirectoryIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native directory identity is unavailable on this platform",
    ))
}

/// Reject non-regular, reparse-point, or multiply linked journal handles.
///
/// The caller retains ownership of the file. This inspects the already opened
/// object; it does not authorize a path or make parent directories trustworthy.
pub fn validate_journal_handle(file: &File) -> io::Result<()> {
    validate(file)
}

#[cfg(unix)]
/// Inspect the live inode rather than trusting an earlier path metadata check.
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
/// Borrow the live handle to inspect link count and reparse attributes via Win32.
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
    use super::{directory_identity, validate_journal_handle};
    use std::fs::{self, File};
    use std::io::Write;

    fn open_directory(path: &std::path::Path) -> File {
        #[cfg(unix)]
        {
            File::open(path).expect("open test directory")
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            let mut options = fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
            options.open(path).expect("open test directory")
        }
    }

    #[test]
    fn directory_identity_is_stable_after_child_writes() {
        let directory = tempfile::tempdir().expect("private temporary directory");
        let handle = open_directory(directory.path());
        let before = directory_identity(&handle).expect("directory identity");

        let child = directory.path().join("child");
        let mut file = File::create(child).expect("create child file");
        file.write_all(b"directory identity is stable")
            .expect("write child file");

        let after = directory_identity(&handle).expect("directory identity after write");
        assert_eq!(after, before);
    }

    #[test]
    fn different_directories_have_different_identities() {
        let first = tempfile::tempdir().expect("first private directory");
        let second = tempfile::tempdir().expect("second private directory");
        let first_handle = open_directory(first.path());
        let second_handle = open_directory(second.path());

        let first_id = directory_identity(&first_handle).expect("first identity");
        let second_id = directory_identity(&second_handle).expect("second identity");
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn directory_identity_rejects_regular_file_handles() {
        let file = tempfile::NamedTempFile::new().expect("private temporary file");
        let error =
            directory_identity(file.as_file()).expect_err("regular file is not a directory");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

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
