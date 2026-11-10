//! Windows handle-relative workspace operations.
//!
//! This module uses the Native API because the safe standard-library and
//! capability-library interfaces do not expose the required no-reparse,
//! directory-handle-relative open contract. It is an I/O boundary only. The
//! core must authorize every operation, and this module does not contain a
//! process or network sandbox.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::{MaybeUninit, offset_of, size_of};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_ID_BOTH_DIR_INFORMATION, FILE_NON_DIRECTORY_FILE,
    FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    FileIdBothDirectoryInformation, NtCreateFile, NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_NO_MORE_FILES,
    STATUS_OBJECT_NAME_COLLISION, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE,
    FILE_WRITE_DATA, FileAttributeTagInfo, FileBasicInfo, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::{DirectoryIdentity, directory_identity};

use super::{CreateError, DirectoryEntry, EntryKind, Listing, RelativePath};

const WINDOWS_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
const DIRECTORY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_COMPONENT_UNITS: usize = 255;
const SHARE_READ_WRITE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const DIRECTORY_ACCESS: FILE_ACCESS_RIGHTS = FILE_LIST_DIRECTORY
    | FILE_TRAVERSE
    | FILE_READ_ATTRIBUTES
    | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;

/// Retains the selected root and workspace handles for the full broker lifetime.
///
/// The handles are opened without delete sharing. This blocks ordinary rename
/// and delete operations while they are held. It does not establish isolation
/// from kernel drivers, hostile filesystems, or other privileged processes.
pub(super) struct NativeWorkspace {
    root: File,
    workspace: File,
    root_path: PathBuf,
    workspace_relative: Vec<OsString>,
    /// Pins every directory used to resolve the workspace at construction.
    workspace_path_handles: Vec<File>,
    root_identity: DirectoryIdentity,
    workspace_identity: DirectoryIdentity,
    root_created: SystemTime,
    workspace_created: SystemTime,
}

impl NativeWorkspace {
    /// Open the trusted root by its configured path, then walk the workspace
    /// below it by one name component per retained directory handle.
    pub(super) fn open(
        root_path: &Path,
        expected_root: DirectoryIdentity,
        workspace_relative: &RelativePath,
        expected_workspace: DirectoryIdentity,
    ) -> io::Result<Self> {
        if expected_root.volume != expected_workspace.volume {
            return Err(invalid_data(
                "workspace is on a different volume from its root",
            ));
        }

        let root = open_configured_root(root_path)?;
        require_directory_identity(&root, expected_root, expected_root.volume)?;
        let root_created = creation_time(&root)?;

        let mut workspace_path_handles = vec![root.try_clone()?];
        for component in &workspace_relative.components {
            validate_component(component)?;
            let parent = last_directory(&workspace_path_handles)?;
            let child = open_relative_directory(parent, component, expected_root.volume)?;
            workspace_path_handles.push(child);
        }
        let workspace = last_directory(&workspace_path_handles)?.try_clone()?;
        require_directory_identity(&workspace, expected_workspace, expected_root.volume)?;
        let workspace_created = creation_time(&workspace)?;

        Ok(Self {
            root,
            workspace,
            root_path: root_path.to_path_buf(),
            workspace_relative: workspace_relative.components.clone(),
            workspace_path_handles,
            root_identity: expected_root,
            workspace_identity: expected_workspace,
            root_created,
            workspace_created,
        })
    }

    /// Return creation times read from the retained handles after rechecking
    /// both directory IDs and timestamps.
    pub(super) fn creation_times(&self) -> io::Result<(SystemTime, SystemTime)> {
        self.ensure_current()?;
        Ok((self.root_created, self.workspace_created))
    }

    /// Read one validated regular single-link file through its retained parent.
    pub(super) fn read(&self, path: &RelativePath, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.ensure_current()?;
        let (parent_handles, leaf) = self.open_parent(path)?;
        let parent = last_directory(&parent_handles)?;
        validate_component(&leaf)?;
        let mut file = open_relative_file(parent, &leaf, self.workspace_identity.volume)?;
        require_file_size(&file, max_bytes)?;
        let data = read_bounded(&mut file, max_bytes)?;
        validate_regular_file(&file, self.workspace_identity.volume)?;
        self.ensure_current()?;
        Ok(data)
    }

    /// Enumerate one opened directory. The listing is a bounded observation,
    /// not a stable snapshot; each entry is reopened relative to this handle
    /// before its descriptive kind is returned.
    pub(super) fn list(
        &self,
        path: &RelativePath,
        max_entries: usize,
        max_name_units: usize,
    ) -> io::Result<Listing> {
        self.ensure_current()?;
        let directory_handles = self.open_directory_path(path)?;
        let directory = last_directory(&directory_handles)?;
        let mut entries = Vec::new();
        let mut name_units = 0usize;
        let mut truncated = false;
        let mut restart = true;

        loop {
            let batch = query_directory_batch(directory, restart)?;
            restart = false;
            if batch.is_empty() {
                break;
            }
            for (name, attributes) in batch {
                if is_dot_entry(&name) {
                    continue;
                }
                if entries.len() == max_entries {
                    truncated = true;
                    break;
                }
                let units = name.encode_wide().count();
                let Some(next_units) = name_units.checked_add(units) else {
                    truncated = true;
                    break;
                };
                if next_units > max_name_units {
                    truncated = true;
                    break;
                }
                name_units = next_units;
                let kind = self.classify_entry(directory, &name, attributes);
                entries.push(DirectoryEntry { name, kind });
            }
            if truncated {
                break;
            }
        }

        self.ensure_current()?;
        Ok(Listing { entries, truncated })
    }

    /// Create a new regular file with FILE_CREATE. The final name is never
    /// overwritten or automatically removed after a partial write or sync.
    /// A successful file sync does not establish that its parent directory
    /// entry survives power loss; this backend does not claim that guarantee.
    pub(super) fn create_new(&self, path: &RelativePath, data: &[u8]) -> Result<(), CreateError> {
        self.ensure_current()
            .map_err(|error| create_error(error, false))?;
        let (parent_handles, leaf) = self
            .open_parent(path)
            .map_err(|error| create_error(error, false))?;
        let parent = last_directory(&parent_handles).map_err(|error| create_error(error, false))?;
        validate_component(&leaf).map_err(|error| create_error(error, false))?;

        let mut file = create_relative(
            parent,
            &leaf,
            FILE_WRITE_DATA
                | FILE_READ_ATTRIBUTES
                | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            0,
            FILE_ATTRIBUTE_NORMAL,
        )?;
        validate_regular_file(&file, self.workspace_identity.volume)
            .map_err(|error| create_error(error, true))?;

        file.write_all(data)
            .map_err(|error| create_error(error, true))?;
        file.sync_all().map_err(|error| create_error(error, true))?;
        self.ensure_current()
            .map_err(|error| create_error(error, true))?;
        Ok(())
    }

    /// Create one directory below an already existing parent. No recursive
    /// creation or cleanup is performed. Windows directory-entry durability
    /// across power loss is not qualified by this backend.
    pub(super) fn create_dir(&self, path: &RelativePath) -> Result<(), CreateError> {
        self.ensure_current()
            .map_err(|error| create_error(error, false))?;
        let (parent_handles, leaf) = self
            .open_parent(path)
            .map_err(|error| create_error(error, false))?;
        let parent = last_directory(&parent_handles).map_err(|error| create_error(error, false))?;
        validate_component(&leaf).map_err(|error| create_error(error, false))?;

        let directory = create_relative(
            parent,
            &leaf,
            DIRECTORY_ACCESS,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            SHARE_READ_WRITE,
            FILE_ATTRIBUTE_DIRECTORY,
        )?;
        validate_directory(&directory, self.workspace_identity.volume)
            .map_err(|error| create_error(error, true))?;
        self.ensure_current()
            .map_err(|error| create_error(error, true))?;
        Ok(())
    }

    fn ensure_current(&self) -> io::Result<()> {
        require_directory_identity(&self.root, self.root_identity, self.root_identity.volume)?;
        require_creation_time(&self.root, self.root_created)?;
        require_directory_identity(
            &self.workspace,
            self.workspace_identity,
            self.root_identity.volume,
        )?;
        for directory in &self.workspace_path_handles {
            validate_directory(directory, self.root_identity.volume)?;
        }
        require_creation_time(&self.workspace, self.workspace_created)
    }

    fn open_directory_path(&self, path: &RelativePath) -> io::Result<Vec<File>> {
        self.open_directory_chain(&path.components)
    }

    fn open_directory_chain(&self, components: &[OsString]) -> io::Result<Vec<File>> {
        let mut handles = self.open_workspace_chain()?;
        for component in components {
            validate_component(component)?;
            let parent = last_directory(&handles)?;
            let child = open_relative_directory(parent, component, self.workspace_identity.volume)?;
            handles.push(child);
        }
        Ok(handles)
    }

    fn open_workspace_chain(&self) -> io::Result<Vec<File>> {
        let mut handles = if self.workspace_relative.is_empty() {
            vec![open_configured_root(&self.root_path)?]
        } else {
            vec![self.root.try_clone()?]
        };
        for component in &self.workspace_relative {
            validate_component(component)?;
            let parent = last_directory(&handles)?;
            let child = open_relative_directory(parent, component, self.root_identity.volume)?;
            handles.push(child);
        }
        let workspace = last_directory(&handles)?;
        require_directory_identity(
            workspace,
            self.workspace_identity,
            self.root_identity.volume,
        )?;
        require_creation_time(workspace, self.workspace_created)?;
        Ok(handles)
    }

    fn open_parent(&self, path: &RelativePath) -> io::Result<(Vec<File>, OsString)> {
        let Some((leaf, parents)) = path.components.split_last() else {
            return Err(invalid_input("operation requires a relative entry name"));
        };
        let handles = self.open_directory_chain(parents)?;
        Ok((handles, leaf.clone()))
    }

    fn classify_entry(&self, parent: &File, name: &OsStr, attributes: u32) -> EntryKind {
        if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE) != 0 {
            return EntryKind::Blocked;
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            if open_relative_directory(parent, name, self.workspace_identity.volume).is_ok() {
                EntryKind::Directory
            } else {
                EntryKind::Blocked
            }
        } else if open_relative_file(parent, name, self.workspace_identity.volume).is_ok() {
            EntryKind::File
        } else {
            EntryKind::Blocked
        }
    }
}

fn last_directory(handles: &[File]) -> io::Result<&File> {
    handles
        .last()
        .ok_or_else(|| invalid_data("directory handle chain is empty"))
}

fn open_configured_root(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid_input("workspace root path must be absolute"));
    }
    let access = DIRECTORY_ACCESS;
    let mut options = OpenOptions::new();
    options
        .access_mode(access)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

fn open_relative_directory(parent: &File, name: &OsStr, volume: u64) -> io::Result<File> {
    let file = open_relative(
        parent,
        name,
        DIRECTORY_ACCESS,
        FILE_OPEN,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        SHARE_READ_WRITE,
        FILE_ATTRIBUTE_DIRECTORY,
    )?;
    validate_directory(&file, volume)?;
    Ok(file)
}

fn open_relative_file(parent: &File, name: &OsStr, volume: u64) -> io::Result<File> {
    let file = open_relative(
        parent,
        name,
        FILE_READ_DATA
            | FILE_READ_ATTRIBUTES
            | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        FILE_SHARE_READ,
        0,
    )?;
    validate_regular_file(&file, volume)?;
    Ok(file)
}

/// Make a single-component open relative to a live directory handle.
///
/// `OBJ_DONT_REPARSE` is required for traversal. `FILE_OPEN_REPARSE_POINT`
/// makes the final object inspectable rather than following its reparse data.
fn open_relative(
    parent: &File,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    disposition: u32,
    options: u32,
    share_access: u32,
    attributes: u32,
) -> io::Result<File> {
    open_relative_inner(
        parent,
        name,
        access,
        disposition,
        options,
        share_access,
        attributes,
    )
    .map_err(|failure| failure.error)
}

fn create_relative(
    parent: &File,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    disposition: u32,
    options: u32,
    share_access: u32,
    attributes: u32,
) -> Result<File, CreateError> {
    open_relative_inner(
        parent,
        name,
        access,
        disposition,
        options,
        share_access,
        attributes,
    )
    .map_err(|failure| create_error(failure.error, failure.may_have_created))
}

struct RelativeOpenFailure {
    error: io::Error,
    may_have_created: bool,
}

#[allow(unsafe_code)]
fn open_relative_inner(
    parent: &File,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    disposition: u32,
    options: u32,
    share_access: u32,
    attributes: u32,
) -> Result<File, RelativeOpenFailure> {
    // Every returned handle must preserve the synchronous buffer-lifetime contract.
    if options & FILE_SYNCHRONOUS_IO_NONALERT == 0
        || access & windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE == 0
        || ![FILE_OPEN, FILE_CREATE].contains(&disposition)
    {
        return Err(RelativeOpenFailure {
            error: invalid_input("native opens require synchronous non-destructive options"),
            may_have_created: false,
        });
    }
    let units = encode_component(name).map_err(|error| RelativeOpenFailure {
        error,
        may_have_created: false,
    })?;
    let byte_len = units
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|len| u16::try_from(len).ok())
        .ok_or_else(|| RelativeOpenFailure {
            error: invalid_input("native component name is too long"),
            may_have_created: false,
        })?;
    let maximum_length = byte_len;
    let mut units = units;
    let mut name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: maximum_length,
        Buffer: units.as_mut_ptr(),
    };
    let attributes_object = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).map_err(|_| RelativeOpenFailure {
            error: invalid_input("native object attributes are too large"),
            may_have_created: false,
        })?,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &mut name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut raw = std::ptr::null_mut();
    let mut io_status = IO_STATUS_BLOCK::default();

    // SAFETY: `parent` owns a live directory handle. The `UNICODE_STRING`
    // points to a stable, checked-length UTF-16 buffer containing exactly one
    // validated component. The object-attributes and I/O-status structures
    // remain alive and writable for this synchronous call. The API receives no
    // borrowed data that outlives the call; successful handle ownership moves
    // into `File` below. No ambient child-path reopen occurs. The documented
    // NtCreateFile contract returns a final success or error status; this code
    // accepts only STATUS_SUCCESS and rejects STATUS_PENDING or other positive
    // values without reading output fields.
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access,
            &attributes_object,
            &mut io_status,
            std::ptr::null(),
            attributes,
            share_access,
            disposition,
            options,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(RelativeOpenFailure {
            error: native_error(status),
            may_have_created: status != STATUS_OBJECT_NAME_COLLISION,
        });
    }
    if raw.is_null() || raw == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(RelativeOpenFailure {
            error: invalid_data("native open returned an invalid handle"),
            may_have_created: true,
        });
    }

    // SAFETY: successful NtCreateFile returned a new owned kernel handle.
    // `File` closes that handle exactly once and does not outlive this wrapper.
    Ok(unsafe { File::from_raw_handle(raw as RawHandle) })
}

fn validate_component(name: &OsStr) -> io::Result<()> {
    let units = encode_component(name)?;
    if units.is_empty()
        || units.len() > MAX_COMPONENT_UNITS
        || units.iter().any(|unit| matches!(*unit, 0 | 47 | 92))
        || units == [46]
        || units == [46, 46]
    {
        return Err(invalid_input("invalid native path component"));
    }
    Ok(())
}

fn encode_component(name: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.len() > MAX_COMPONENT_UNITS
        || units.iter().any(|unit| matches!(*unit, 0 | 47 | 92))
        || units == [46]
        || units == [46, 46]
    {
        return Err(invalid_input("invalid native path component"));
    }
    Ok(units)
}

fn require_directory_identity(
    file: &File,
    expected: DirectoryIdentity,
    volume: u64,
) -> io::Result<()> {
    let identity = directory_identity(file)?;
    if identity != expected || identity.volume != volume {
        return Err(invalid_data(
            "opened directory identity does not match the selected workspace",
        ));
    }
    validate_directory(file, volume)
}

fn validate_directory(file: &File, volume: u64) -> io::Result<()> {
    let attributes = query_attributes(file)?;
    let standard = query_standard(file)?;
    let identity = query_identity(file)?;
    if attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || attributes.FileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE) != 0
        || !standard.Directory
        || standard.DeletePending
        || identity.VolumeSerialNumber != volume
    {
        return Err(invalid_data(
            "opened object is not an ordinary directory on the workspace volume",
        ));
    }
    Ok(())
}

fn validate_regular_file(file: &File, volume: u64) -> io::Result<()> {
    let attributes = query_attributes(file)?;
    let standard = query_standard(file)?;
    let identity = query_identity(file)?;
    if attributes.FileAttributes
        & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE)
        != 0
        || standard.Directory
        || standard.DeletePending
        || standard.NumberOfLinks != 1
        || identity.VolumeSerialNumber != volume
    {
        return Err(invalid_data(
            "opened object is not a single-link regular file on the workspace volume",
        ));
    }
    Ok(())
}

fn require_file_size(file: &File, max_bytes: usize) -> io::Result<()> {
    let size = query_standard(file)?.EndOfFile;
    let size = u64::try_from(size)
        .map_err(|_| invalid_data("native file size is negative or out of range"))?;
    let limit =
        u64::try_from(max_bytes).map_err(|_| invalid_input("file read limit is too large"))?;
    if size > limit {
        return Err(invalid_data("file exceeds the requested read limit"));
    }
    Ok(())
}

fn read_bounded(reader: impl Read, max_bytes: usize) -> io::Result<Vec<u8>> {
    let read_limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| invalid_input("file read limit is too large"))?;
    let mut data = Vec::new();
    reader.take(read_limit).read_to_end(&mut data)?;
    if data.len() > max_bytes {
        return Err(invalid_data("file exceeds the requested read limit"));
    }
    Ok(data)
}

/// # Safety
/// `T` must be the exact ABI output structure for `class`.
#[allow(unsafe_code)]
unsafe fn query_info<T: Copy>(file: &File, class: i32) -> io::Result<T> {
    let length = u32::try_from(size_of::<T>())
        .map_err(|_| invalid_input("native metadata structure is too large"))?;
    let mut output = MaybeUninit::<T>::uninit();
    // SAFETY: the live file handle is borrowed for the synchronous call. The
    // output points to properly aligned, writable storage of `size_of::<T>()`.
    // Each caller pairs `T` with its matching information class. A success
    // status initializes the full structure; errors return before field reads.
    // SAFETY: guaranteed by this function's caller contract; `length` is the
    // full size of `T`, which is writable output storage for `class`.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            class,
            output.as_mut_ptr().cast(),
            length,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the API returned success for the exact structure size above.
    // SAFETY: guaranteed by this function's caller contract and API success.
    Ok(unsafe { output.assume_init() })
}

#[allow(unsafe_code)]
fn query_attributes(
    file: &File,
) -> io::Result<windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_TAG_INFO> {
    // SAFETY: `FileAttributeTagInfo` returns exactly `FILE_ATTRIBUTE_TAG_INFO`.
    unsafe { query_info(file, FileAttributeTagInfo) }
}

#[allow(unsafe_code)]
fn query_standard(file: &File) -> io::Result<FILE_STANDARD_INFO> {
    // SAFETY: `FileStandardInfo` returns exactly `FILE_STANDARD_INFO`.
    unsafe { query_info(file, FileStandardInfo) }
}

#[allow(unsafe_code)]
fn query_identity(file: &File) -> io::Result<FILE_ID_INFO> {
    // SAFETY: `FileIdInfo` returns exactly `FILE_ID_INFO`.
    unsafe { query_info(file, FileIdInfo) }
}

#[allow(unsafe_code)]
fn query_basic(file: &File) -> io::Result<FILE_BASIC_INFO> {
    // SAFETY: `FileBasicInfo` returns exactly `FILE_BASIC_INFO`.
    unsafe { query_info(file, FileBasicInfo) }
}

fn creation_time(file: &File) -> io::Result<SystemTime> {
    let info = query_basic(file)?;
    filetime_ticks_to_system_time(info.CreationTime)
}

fn require_creation_time(file: &File, expected: SystemTime) -> io::Result<()> {
    if creation_time(file)? != expected {
        return Err(invalid_data("opened directory creation time changed"));
    }
    Ok(())
}

fn filetime_ticks_to_system_time(ticks: i64) -> io::Result<SystemTime> {
    let difference = ticks
        .checked_sub(WINDOWS_EPOCH_TICKS)
        .ok_or_else(|| invalid_data("native creation time is out of range"))?;
    let duration_nanos = difference
        .unsigned_abs()
        .checked_mul(100)
        .ok_or_else(|| invalid_data("native creation time is out of range"))?;
    let duration = Duration::from_nanos(duration_nanos);
    if difference >= 0 {
        SystemTime::UNIX_EPOCH
            .checked_add(duration)
            .ok_or_else(|| invalid_data("native creation time is out of range"))
    } else {
        SystemTime::UNIX_EPOCH
            .checked_sub(duration)
            .ok_or_else(|| invalid_data("native creation time is out of range"))
    }
}

#[allow(unsafe_code)]
fn query_directory_batch(directory: &File, restart: bool) -> io::Result<Vec<(OsString, u32)>> {
    let units = DIRECTORY_BUFFER_BYTES / size_of::<u64>();
    let mut storage = vec![0u64; units];
    let mut io_status = IO_STATUS_BLOCK::default();
    let length = u32::try_from(DIRECTORY_BUFFER_BYTES)
        .map_err(|_| invalid_input("directory query buffer is too large"))?;

    // SAFETY: `directory` is a live synchronous directory handle. All handles
    // in this module have SYNCHRONIZE access and were opened with
    // FILE_SYNCHRONOUS_IO_NONALERT, or by non-overlapped standard CreateFile.
    // Microsoft documents all I/O through such a file object as synchronous.
    // `storage` is aligned and writable, and the output status block is live.
    // The event and APC pointers are null. Only STATUS_SUCCESS or the explicit
    // end-of-enumeration status is accepted before using returned bytes.
    let status = unsafe {
        NtQueryDirectoryFile(
            directory.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            None,
            std::ptr::null(),
            &mut io_status,
            storage.as_mut_ptr().cast(),
            length,
            FileIdBothDirectoryInformation,
            false,
            std::ptr::null(),
            restart,
        )
    };
    if status == STATUS_NO_MORE_FILES {
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(native_error(status));
    }
    if io_status.Information == 0 {
        return Ok(Vec::new());
    }
    if io_status.Information > DIRECTORY_BUFFER_BYTES {
        return Err(invalid_data(
            "native directory query returned an invalid byte count",
        ));
    }
    let byte_count = io_status.Information;
    // SAFETY: the native call wrote at most `DIRECTORY_BUFFER_BYTES` bytes to
    // the initialized `u64` storage. The byte count was checked above and the
    // borrowed slice cannot outlive that storage.
    let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), byte_count) };
    parse_directory_batch(bytes)
}

fn parse_directory_batch(bytes: &[u8]) -> io::Result<Vec<(OsString, u32)>> {
    // This WDK structure is the exact information class requested above.
    let fixed = offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileName);
    let mut result = Vec::new();
    let mut offset = 0usize;
    loop {
        let fixed_end = offset
            .checked_add(fixed)
            .ok_or_else(|| invalid_data("directory entry offset overflow"))?;
        if fixed_end > bytes.len() {
            return Err(invalid_data("native directory entry is truncated"));
        }
        let next_offset = usize::try_from(read_u32_field(
            bytes,
            offset,
            offset_of!(FILE_ID_BOTH_DIR_INFORMATION, NextEntryOffset),
        )?)
        .map_err(|_| invalid_data("directory entry offset is out of range"))?;
        let name_bytes = usize::try_from(read_u32_field(
            bytes,
            offset,
            offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileNameLength),
        )?)
        .map_err(|_| invalid_data("directory name length is out of range"))?;
        let attributes = read_u32_field(
            bytes,
            offset,
            offset_of!(FILE_ID_BOTH_DIR_INFORMATION, FileAttributes),
        )?;
        if !name_bytes.is_multiple_of(size_of::<u16>()) {
            return Err(invalid_data("native directory name has an odd byte length"));
        }
        let name_end = fixed_end
            .checked_add(name_bytes)
            .ok_or_else(|| invalid_data("native directory name length overflow"))?;
        if name_end > bytes.len() || name_bytes / 2 > MAX_COMPONENT_UNITS {
            return Err(invalid_data("native directory name exceeds its bounds"));
        }
        let mut wide = Vec::with_capacity(name_bytes / 2);
        for pair in bytes[fixed_end..name_end].as_chunks::<2>().0 {
            wide.push(u16::from_le_bytes([pair[0], pair[1]]));
        }
        let name = OsString::from_wide(&wide);
        if !is_dot_entry(&name) {
            validate_component(&name)?;
        }
        result.push((name, attributes));

        if next_offset == 0 {
            break;
        }
        let minimum_next = fixed
            .checked_add(name_bytes)
            .ok_or_else(|| invalid_data("native directory entry size overflow"))?;
        if next_offset < minimum_next || !next_offset.is_multiple_of(8) {
            return Err(invalid_data("native directory entry offset is invalid"));
        }
        offset = offset
            .checked_add(next_offset)
            .ok_or_else(|| invalid_data("native directory entry offset overflow"))?;
        if offset >= bytes.len() {
            return Err(invalid_data("native directory entry chain is truncated"));
        }
    }
    Ok(result)
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset
        .checked_add(size_of::<u32>())
        .ok_or_else(|| invalid_data("native directory field offset overflow"))?;
    let field = bytes
        .get(offset..end)
        .ok_or_else(|| invalid_data("native directory field is truncated"))?;
    Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_u32_field(bytes: &[u8], base: usize, field_offset: usize) -> io::Result<u32> {
    let offset = base
        .checked_add(field_offset)
        .ok_or_else(|| invalid_data("native directory field offset overflow"))?;
    read_u32(bytes, offset)
}

fn is_dot_entry(name: &OsStr) -> bool {
    let units: Vec<u16> = name.encode_wide().collect();
    units == [46] || units == [46, 46]
}

fn native_error(status: NTSTATUS) -> io::Error {
    if status == STATUS_OBJECT_NAME_COLLISION {
        return io::Error::new(
            io::ErrorKind::AlreadyExists,
            "workspace entry already exists",
        );
    }
    io::Error::other(format!(
        "Windows native file operation failed (NTSTATUS 0x{:08X})",
        status as u32
    ))
}

fn create_error(error: io::Error, may_have_created: bool) -> CreateError {
    CreateError {
        error,
        may_have_created,
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn directory_handle(path: &Path) -> File {
        open_configured_root(path).expect("open test directory")
    }

    fn identity(path: &Path) -> DirectoryIdentity {
        directory_identity(&directory_handle(path)).expect("read test directory identity")
    }

    fn native(root: &Path) -> NativeWorkspace {
        let workspace_path = root.join("workspace");
        fs::create_dir_all(&workspace_path).expect("create workspace");
        NativeWorkspace::open(
            root,
            identity(root),
            &RelativePath::new(Path::new("workspace")).expect("relative workspace"),
            identity(&workspace_path),
        )
        .expect("open anchored workspace")
    }

    #[test]
    fn creates_reads_lists_and_creates_directories_through_handles() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let subdirectory = RelativePath::new(Path::new("notes")).expect("directory name");
        workspace
            .create_dir(&subdirectory)
            .expect("create directory");

        let file = RelativePath::new(Path::new("notes/entry.txt")).expect("file path");
        workspace
            .create_new(&file, b"bounded broker")
            .expect("create file");
        assert_eq!(
            workspace.read(&file, 64).expect("read file"),
            b"bounded broker"
        );

        let listing = workspace
            .list(&subdirectory, 8, 128)
            .expect("list child directory");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, OsString::from("entry.txt"));
        assert_eq!(listing.entries[0].kind, EntryKind::File);
        assert!(!listing.truncated);

        let short_listing = workspace
            .list(&subdirectory, 1, 2)
            .expect("list with a short name budget");
        assert!(short_listing.entries.is_empty());
        assert!(short_listing.truncated);
    }

    #[test]
    fn repeated_listings_of_root_workspace_use_independent_cursors() {
        let temporary = tempdir().expect("temporary directory");
        let root = temporary.path();
        let root_identity = identity(root);
        let workspace = NativeWorkspace::open(
            root,
            root_identity,
            &RelativePath::new(Path::new("")).expect("empty workspace-relative path"),
            root_identity,
        )
        .expect("open root as workspace");
        workspace
            .create_new(
                &RelativePath::new(Path::new("first.txt")).expect("first file"),
                b"first",
            )
            .expect("create first entry");
        workspace
            .create_new(
                &RelativePath::new(Path::new("second.txt")).expect("second file"),
                b"second",
            )
            .expect("create second entry");

        let path = RelativePath::new(Path::new("")).expect("root listing path");
        let first = workspace.list(&path, 8, 128).expect("first listing");
        let second = workspace.list(&path, 8, 128).expect("second listing");
        let first_names: Vec<_> = first.entries.iter().map(|entry| &entry.name).collect();
        let second_names: Vec<_> = second.entries.iter().map(|entry| &entry.name).collect();
        assert_eq!(first_names, second_names);
        assert_eq!(first_names.len(), 2);
        assert!(!first.truncated);
        assert!(!second.truncated);
    }

    #[test]
    fn new_file_collision_does_not_claim_a_creation() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let file = RelativePath::new(Path::new("exists.txt")).expect("file path");
        workspace
            .create_new(&file, b"first")
            .expect("initial create");
        let error = workspace
            .create_new(&file, b"replacement")
            .expect_err("collision");
        assert_eq!(error.error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!error.may_have_created);
        assert_eq!(
            workspace.read(&file, 64).expect("original remains"),
            b"first"
        );
    }

    #[test]
    fn read_limit_is_checked_before_and_during_read() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let file = RelativePath::new(Path::new("large.txt")).expect("file path");
        workspace.create_new(&file, b"12345").expect("create file");
        let error = workspace.read(&file, 4).expect_err("oversized read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let mut cursor = std::io::Cursor::new(b"12345678901234567890");
        let error = read_bounded(&mut cursor, 4)
            .expect_err("read-time bound catches growth after metadata preflight");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(cursor.position(), 5, "consume only the limit plus one byte");
    }

    #[test]
    fn retained_creation_times_stay_stable_after_child_writes() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let before = workspace.creation_times().expect("creation times");
        let file = RelativePath::new(Path::new("child.txt")).expect("file path");
        workspace
            .create_new(&file, b"child write")
            .expect("create child");
        assert_eq!(workspace.creation_times().expect("creation times"), before);
    }

    #[test]
    fn hard_link_is_blocked_from_reads_and_listed_as_blocked() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let external = temporary.path().join("outside.txt");
        fs::write(&external, b"outside").expect("create external file");
        let linked = temporary.path().join("workspace").join("linked.txt");
        fs::hard_link(&external, &linked).expect("create hard link");

        let relative = RelativePath::new(Path::new("linked.txt")).expect("link name");
        assert!(workspace.read(&relative, 64).is_err());
        let listing = workspace
            .list(
                &RelativePath::new(Path::new("")).expect("workspace root"),
                8,
                128,
            )
            .expect("list workspace");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].kind, EntryKind::Blocked);
    }

    #[test]
    fn an_existing_writer_causes_a_controlled_read_refusal() {
        use std::os::windows::fs::OpenOptionsExt;

        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let full_path = temporary.path().join("workspace").join("shared.txt");
        fs::write(&full_path, b"stable bytes").expect("create test file");
        let mut writer_options = OpenOptions::new();
        writer_options.read(true).write(true).share_mode(
            FILE_SHARE_READ
                | FILE_SHARE_WRITE
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
        );
        let writer = writer_options.open(&full_path).expect("hold writer handle");

        let relative = RelativePath::new(Path::new("shared.txt")).expect("relative file");
        assert!(workspace.read(&relative, 64).is_err());
        drop(writer);
        assert_eq!(
            workspace
                .read(&relative, 64)
                .expect("read after writer closes"),
            b"stable bytes"
        );
    }

    #[test]
    fn retained_handles_block_rename_of_a_workspace_ancestor() {
        let temporary = tempdir().expect("temporary directory");
        let parent_path = temporary.path().join("parent");
        let workspace_path = parent_path.join("workspace");
        fs::create_dir_all(&workspace_path).expect("create workspace path");
        let native = NativeWorkspace::open(
            temporary.path(),
            identity(temporary.path()),
            &RelativePath::new(Path::new("parent/workspace")).expect("workspace path"),
            identity(&workspace_path),
        )
        .expect("open anchored workspace");
        let renamed = temporary.path().join("parent-renamed");

        assert!(fs::rename(&parent_path, &renamed).is_err());
        assert!(parent_path.is_dir());
        assert!(!renamed.exists());
        assert_eq!(
            native.creation_times().expect("retained identities").1,
            creation_time(&native.workspace).expect("workspace creation time")
        );
    }

    #[test]
    fn non_ascii_native_names_round_trip_without_lossy_conversion() {
        let temporary = tempdir().expect("temporary directory");
        let workspace = native(temporary.path());
        let name = OsString::from("résumé-東京.txt");
        let file = RelativePath::new(Path::new(&name)).expect("native name");
        workspace
            .create_new(&file, b"unicode")
            .expect("create unicode file");
        assert_eq!(
            workspace.read(&file, 64).expect("read unicode file"),
            b"unicode"
        );
        let listing = workspace
            .list(
                &RelativePath::new(Path::new("")).expect("workspace root"),
                8,
                128,
            )
            .expect("list workspace");
        assert_eq!(listing.entries[0].name, name);
    }

    #[test]
    fn workspace_identity_mismatch_fails_during_open() {
        let temporary = tempdir().expect("temporary directory");
        let workspace_path = temporary.path().join("workspace");
        fs::create_dir(&workspace_path).expect("create workspace");
        let other_path = temporary.path().join("other");
        fs::create_dir(&other_path).expect("create other directory");
        let error = NativeWorkspace::open(
            temporary.path(),
            identity(temporary.path()),
            &RelativePath::new(Path::new("workspace")).expect("workspace path"),
            identity(&other_path),
        )
        .err()
        .expect("wrong workspace identity");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn junction_listing_read_and_create_are_blocked() {
        let temporary = tempdir().expect("temporary directory");
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        let workspace = native(temporary.path());
        let link = temporary.path().join("workspace").join("redirect");
        create_junction_bounded(&link, &outside).expect("create a real NTFS junction fixture");
        assert!(
            fs::symlink_metadata(&link)
                .expect("junction metadata")
                .file_type()
                .is_symlink()
        );

        let listing = workspace
            .list(
                &RelativePath::new(Path::new("")).expect("workspace root"),
                8,
                128,
            )
            .expect("list workspace");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].kind, EntryKind::Blocked);
        assert!(
            workspace
                .list(
                    &RelativePath::new(Path::new("redirect")).expect("link path"),
                    8,
                    128
                )
                .is_err()
        );

        let sentinel = outside.join("sentinel.txt");
        fs::write(&sentinel, b"outside remains unchanged").expect("outside sentinel");
        let through_link =
            RelativePath::new(Path::new("redirect/sentinel.txt")).expect("junction child path");
        assert!(workspace.read(&through_link, 128).is_err());
        let create_through_link =
            RelativePath::new(Path::new("redirect/new.txt")).expect("junction create path");
        let create_error = workspace
            .create_new(&create_through_link, b"must not escape")
            .expect_err("junction write is refused");
        assert!(!create_error.may_have_created);
        assert_eq!(
            fs::read(&sentinel).expect("outside sentinel remains"),
            b"outside remains unchanged"
        );
        assert!(!outside.join("new.txt").exists());
    }

    fn create_junction_bounded(link: &Path, target: &Path) -> io::Result<()> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::Instant;

        let link = link
            .to_str()
            .ok_or_else(|| invalid_input("junction path is not valid UTF-16 text"))?;
        let target = target
            .to_str()
            .ok_or_else(|| invalid_input("junction target is not valid UTF-16 text"))?;
        if [link, target].iter().any(|path| {
            path.contains(['%', '"', '\r', '\n', '&', '|', '<', '>', '^', '!', '(', ')'])
        }) {
            return Err(invalid_input(
                "test junction path contains a cmd.exe expansion character",
            ));
        }
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J", link, target])
            // CREATE_NO_WINDOW keeps the fixture helper hidden and noninteractive.
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(wait_error) => {
                    terminate_and_reap(&mut child)?;
                    return Err(wait_error);
                }
            };
            if let Some(status) = status {
                return if status.success() {
                    Ok(())
                } else {
                    let output = child.wait_with_output()?;
                    Err(io::Error::other(format!(
                        "mklink /J failed with {status}: stdout={:?}, stderr={:?}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )))
                };
            }
            if Instant::now() >= deadline {
                terminate_and_reap(&mut child)?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "mklink /J did not finish within five seconds",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate_and_reap(child: &mut std::process::Child) -> io::Result<()> {
        use std::thread;
        use std::time::Instant;

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            if let Err(kill_error) = child.kill() {
                if child.try_wait()?.is_some() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other(format!(
                        "could not terminate timed-out mklink fixture: {kill_error}"
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed-out mklink fixture did not exit after termination",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn malformed_native_components_are_rejected() {
        for units in [
            Vec::new(),
            vec![0],
            vec![46],
            vec![46, 46],
            vec![97, 47, 98],
            vec![97, 92, 98],
        ] {
            let name = OsString::from_wide(&units);
            assert!(validate_component(&name).is_err(), "{units:?}");
        }
        let too_long = OsString::from_wide(&vec![120; MAX_COMPONENT_UNITS + 1]);
        assert!(validate_component(&too_long).is_err());
    }
}
