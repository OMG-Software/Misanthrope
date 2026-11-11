//! Windows NTFS-only handle-relative replacement publication.
//!
//! The retained source handle denies write sharing, so ordinary writers are
//! blocked for the whole operation. It permits delete sharing because the final
//! rename must replace that name. A removal or swap after the last check can
//! therefore still go undetected.
//!
//! The stage keeps the private store's owner-only DACL until the commit
//! attempt. Windows grants bypass traverse checking by default, so private
//! parent directories alone would not hide a stage whose own DACL admits other
//! principals. See ADR-0024.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::time::SystemTime;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN, FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
    FileRenameInformationEx, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    HANDLE, NTSTATUS, STATUS_DELETE_PENDING, STATUS_FILE_IS_A_DIRECTORY, STATUS_NOT_A_DIRECTORY,
    STATUS_OBJECT_NAME_NOT_FOUND, STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_SHARING_VIOLATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_NAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FileNormalizedNameInfo, GetFileInformationByHandleEx, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{
    DIRECTORY_ACCESS, NativeStatus, NativeWorkspace, RelativePath, SHARE_READ_WRITE, creation_time,
    encode_component, invalid_data, invalid_input, last_directory, native_error, open_relative,
    query_attributes, query_basic, query_identity, query_standard, read_bounded,
    validate_component, validate_directory, validate_regular_file,
};
use crate::broker::{FileCapture, MAX_FILE_BYTES};
use crate::directory_identity;
use crate::private_store::windows::{
    DIRECTORY_OPTIONS, FILE_OPTIONS, FILE_READ_ACCESS, STAGE_NAME, apply_native_security,
    is_private_file, read_native_security, require_ntfs, validate_file_metadata,
};
use crate::private_store::{
    NativeSecurity, PrivateDirectory, PrivateFileIdentity, StageMetadata, stage_metadata,
};

const SHARE_ALL: u32 = SHARE_READ_WRITE | FILE_SHARE_DELETE;
const NAME_BUFFER_BYTES: usize = 64 * 1024 + 16;

/// A retained existing workspace entry and its bounded expected version.
pub struct ReplacementTarget<'a> {
    workspace: &'a NativeWorkspace,
    path: RelativePath,
    /// Pins every directory on the path. These handles deny delete sharing.
    handles: Vec<File>,
    /// The same parent, reopened with the access rename and flush need.
    parent: File,
    name: OsString,
    source: File,
    source_snapshot: FileSnapshot,
    parent_created: SystemTime,
    capture: FileCapture,
    #[cfg(test)]
    hook: Option<std::sync::Arc<dyn Fn(TestPoint) -> io::Result<()> + Send + Sync>>,
}

/// An exclusively created proposed file in private operation storage.
pub struct ReplacementStage {
    directory: PrivateDirectory,
    file: File,
    proposed: Vec<u8>,
    metadata: StageMetadata,
    source_snapshot: FileSnapshot,
    commit_started: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestPoint {
    AfterFinalCheck,
    Rename,
    PublishedCheck,
    StageSync,
    SourceParentSync,
    TargetParentSync,
    RestorePrivacy,
}

/// One object and version. Access time is excluded because reads change it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshot {
    volume: u64,
    file_id: [u8; 16],
    length: i64,
    last_write: i64,
    change_time: i64,
    attributes: u32,
}

/// A source pinned by a read-only handle, with bytes read from that handle.
struct PinnedSource {
    handles: Vec<File>,
    name: OsString,
    source: File,
    snapshot: FileSnapshot,
    capture: FileCapture,
}

impl NativeWorkspace {
    /// Capture bytes and security from one retained handle, for a checkpoint.
    pub(in crate::broker) fn capture(
        &self,
        path: &RelativePath,
        max_bytes: usize,
    ) -> io::Result<FileCapture> {
        Ok(self.pin_source(path, max_bytes)?.capture)
    }

    pub(in crate::broker) fn replacement_target<'a>(
        &'a self,
        path: &RelativePath,
        max_bytes: usize,
    ) -> io::Result<ReplacementTarget<'a>> {
        let pinned = self.pin_source(path, max_bytes)?;
        let directory = last_directory(&pinned.handles)?;
        // Refuse a target that the final rename could not remove, such as one
        // held open without delete sharing, before any private artifact exists.
        drop(open_relative(
            directory,
            &pinned.name,
            DELETE | SYNCHRONIZE,
            FILE_OPEN,
            FILE_OPTIONS,
            SHARE_ALL,
            0,
        )?);
        let parent = open_relative(
            directory,
            OsStr::new(""),
            DIRECTORY_ACCESS | FILE_ADD_FILE,
            FILE_OPEN,
            DIRECTORY_OPTIONS,
            SHARE_READ_WRITE,
            0,
        )?;
        validate_directory(&parent, pinned.snapshot.volume)?;
        if directory_identity(&parent)? != directory_identity(directory)? {
            return Err(invalid_data("reopened parent differs from retained parent"));
        }
        let parent_created = creation_time(&parent)?;
        Ok(ReplacementTarget {
            workspace: self,
            path: path.clone(),
            handles: pinned.handles,
            parent,
            name: pinned.name,
            source: pinned.source,
            source_snapshot: pinned.snapshot,
            parent_created,
            capture: pinned.capture,
            #[cfg(test)]
            hook: None,
        })
    }

    fn pin_source(&self, path: &RelativePath, max_bytes: usize) -> io::Result<PinnedSource> {
        self.ensure_current()?;
        let (handles, name) = self.open_parent(path)?;
        validate_component(&name)?;
        let directory = last_directory(&handles)?;
        require_ntfs(directory)?;
        // Denying write sharing also fails here if a writer already has it open.
        let source = open_relative(
            directory,
            &name,
            FILE_READ_ACCESS,
            FILE_OPEN,
            FILE_OPTIONS,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            0,
        )?;
        let snapshot = file_snapshot(&source)?;
        if snapshot.volume != self.workspace_identity.volume {
            return Err(invalid_data("replacement source is on another volume"));
        }
        let limit = i64::try_from(max_bytes)
            .map_err(|_| invalid_input("replacement capture limit cannot be represented"))?;
        if snapshot.length > limit {
            return Err(invalid_data("replacement source exceeds the capture limit"));
        }
        require_exact_name(&source, &name)?;
        validate_file_metadata(&source)?;
        let security = read_native_security(&source)?;
        let length = usize::try_from(snapshot.length)
            .map_err(|_| invalid_data("replacement source length cannot be represented"))?;
        let bytes = read_from_start(&source, length)?;
        validate_file_metadata(&source)?;
        if bytes.len() != length
            || file_snapshot(&source)? != snapshot
            || read_native_security(&source)? != security
        {
            return Err(changed_during_operation());
        }
        self.ensure_current()?;
        Ok(PinnedSource {
            handles,
            name,
            source,
            snapshot,
            capture: FileCapture {
                bytes,
                volume: snapshot.volume,
                file_id: snapshot.file_id,
                security,
            },
        })
    }
}

impl ReplacementTarget<'_> {
    /// Captured bytes and metadata from the pinned source object.
    pub fn capture(&self) -> &FileCapture {
        &self.capture
    }

    /// Check the target volume and flush support before private artifacts.
    pub fn check_storage(&self, directory: &PrivateDirectory) -> io::Result<()> {
        validate_same_volume(
            self.source_snapshot.volume,
            directory_identity(&self.parent)?.volume,
            directory.identity()?.volume,
        )?;
        // Probe the exact retained target parent before any operation artifacts.
        self.parent.sync_all()
    }

    /// Exclusively create, flush, and verify one bounded private stage file.
    ///
    /// The stage keeps its owner-only DACL. It takes the source's security and
    /// indexing opt-out only in `commit`, after the final check.
    pub fn prepare_stage(
        &self,
        directory: PrivateDirectory,
        proposed: &[u8],
    ) -> io::Result<ReplacementStage> {
        if proposed.len() > MAX_FILE_BYTES {
            return Err(invalid_input("replacement proposal exceeds 1 MiB"));
        }
        self.check_storage(&directory)?;
        let mut file = directory.create_stage()?;
        file.write_all(proposed)?;
        file.sync_all()?;
        directory.sync()?;
        let snapshot = file_snapshot(&file)?;
        let metadata = stage_metadata(&file)?;
        if !is_private_file(&file)? || !file_bytes_matches(&file, proposed)? {
            return Err(invalid_data(
                "replacement stage metadata or bytes do not match",
            ));
        }
        if stage_metadata(&file)? != metadata || file_snapshot(&file)? != snapshot {
            return Err(invalid_data(
                "replacement stage metadata changed during preparation",
            ));
        }
        Ok(ReplacementStage {
            directory,
            file,
            proposed: proposed.to_vec(),
            metadata,
            source_snapshot: self.source_snapshot,
            commit_started: false,
        })
    }

    /// Recheck target and stage without changing either object.
    pub fn recheck(&self, stage: &ReplacementStage) -> io::Result<bool> {
        if stage.commit_started {
            return Err(invalid_input(
                "replacement stage attempt was already consumed",
            ));
        }
        self.check_objects(stage)
    }

    fn check_objects(&self, stage: &ReplacementStage) -> io::Result<bool> {
        if self.source_snapshot != stage.source_snapshot
            || stage.directory.identity()?.volume != self.source_snapshot.volume
        {
            return Err(invalid_input(
                "stage does not belong to the retained source and volume",
            ));
        }
        validate_stage(stage)?;
        self.target_matches_current()
    }

    fn target_matches_current(&self) -> io::Result<bool> {
        if !snapshot_matches(&self.source, self.source_snapshot)?
            || !self.parent_matches_registration()?
        {
            return Ok(false);
        }
        let Some(current) = self.observe_target()? else {
            return Ok(false);
        };
        Ok(snapshot_matches(&current, self.source_snapshot)?
            && has_exact_name(&current, &self.name)?
            && metadata_is_supported(&current)?
            && security_matches(&current, &self.capture.security)?
            && file_bytes_matches(&current, &self.capture.bytes)?
            && snapshot_matches(&current, self.source_snapshot)?)
    }

    /// Open whatever the name now refers to, or report that it has changed.
    fn observe_target(&self) -> io::Result<Option<File>> {
        match open_relative(
            last_directory(&self.handles)?,
            &self.name,
            FILE_READ_ACCESS,
            FILE_OPEN,
            FILE_OPTIONS,
            SHARE_ALL,
            0,
        ) {
            Ok(file) => Ok(Some(file)),
            Err(error) if is_observed_change(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn parent_matches_registration(&self) -> io::Result<bool> {
        if let Err(error) = self.workspace.ensure_current() {
            return observed_false(error);
        }
        let handles = match self.workspace.open_parent(&self.path) {
            Ok((handles, _)) => handles,
            Err(error) => return observed_false(error),
        };
        let current = last_directory(&handles)?;
        Ok(
            directory_identity(current)? == directory_identity(&self.parent)?
                && creation_time(current)? == self.parent_created,
        )
    }

    /// Make one native replacement attempt. Every error is an uncertain result.
    pub fn commit(
        &self,
        stage: &mut ReplacementStage,
        before_attempt: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if stage.commit_started {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "replacement stage commit was already attempted",
            ));
        }
        stage.commit_started = true;
        if !self.check_objects(stage)? {
            return Err(changed_during_operation());
        }
        #[cfg(test)]
        self.run_hook(TestPoint::AfterFinalCheck)?;
        let private = stage.metadata.security.clone();
        let attempt = self.publish_security(stage).and_then(|()| {
            before_attempt()?;
            #[cfg(test)]
            self.run_hook(TestPoint::Rename)?;
            rename_onto(&stage.file, &self.parent, &self.name)
        });
        if let Err(error) = attempt {
            return Err(match self.restore_privacy(stage, &private) {
                Ok(()) => error,
                Err(restore) => io::Error::new(
                    error.kind(),
                    format!("{error}; the stage keeps the source security: {restore}"),
                ),
            });
        }
        #[cfg(test)]
        self.run_hook(TestPoint::PublishedCheck)?;
        if !published_matches(self, stage)? {
            return Err(invalid_data(
                "published replacement does not match the staged object",
            ));
        }
        #[cfg(test)]
        self.run_hook(TestPoint::StageSync)?;
        stage.file.sync_all()?;
        #[cfg(test)]
        self.run_hook(TestPoint::SourceParentSync)?;
        stage.directory.sync()?;
        #[cfg(test)]
        self.run_hook(TestPoint::TargetParentSync)?;
        self.parent.sync_all()
    }

    /// Give the verified private stage the captured source security and
    /// indexing opt-out, flush it, and read the result back exactly.
    fn publish_security(&self, stage: &mut ReplacementStage) -> io::Result<()> {
        apply_native_security(&stage.file, &self.capture.security)?;
        stage.file.sync_all()?;
        let metadata = stage_metadata(&stage.file)?;
        if metadata.identity != stage.metadata.identity
            || metadata.security != self.capture.security
        {
            return Err(invalid_data(
                "replacement stage did not take the source security",
            ));
        }
        stage.metadata = metadata;
        Ok(())
    }

    /// Put back the owner-only DACL after a failed attempt left the name
    /// unchanged. Only the stage changes. The core accepts either state after
    /// commit intent.
    fn restore_privacy(
        &self,
        stage: &mut ReplacementStage,
        private: &NativeSecurity,
    ) -> io::Result<()> {
        #[cfg(test)]
        self.run_hook(TestPoint::RestorePrivacy)?;
        apply_native_security(&stage.file, private)?;
        stage.file.sync_all()?;
        let metadata = stage_metadata(&stage.file)?;
        if !is_private_file(&stage.file)? || metadata.security != *private {
            return Err(invalid_data("replacement stage privacy was not restored"));
        }
        stage.metadata = metadata;
        Ok(())
    }

    #[cfg(test)]
    fn run_hook(&self, point: TestPoint) -> io::Result<()> {
        if let Some(hook) = &self.hook {
            hook(point)?;
        }
        Ok(())
    }
}

impl ReplacementStage {
    /// Identity of the same opened stage file.
    pub fn identity(&self) -> io::Result<PrivateFileIdentity> {
        Ok(stage_metadata(&self.file)?.identity)
    }
}

fn validate_stage(stage: &ReplacementStage) -> io::Result<()> {
    let opened = stage.directory.open_stage()?;
    let initial = file_snapshot(&opened)?;
    if stage_metadata(&opened)? != stage.metadata {
        return Err(invalid_data("replacement stage metadata changed"));
    }
    if !file_bytes_matches(&opened, &stage.proposed)? {
        return Err(invalid_data("replacement stage bytes changed"));
    }
    if file_snapshot(&opened)? != initial || stage_metadata(&opened)? != stage.metadata {
        return Err(invalid_data(
            "replacement stage changed during verification",
        ));
    }
    Ok(())
}

fn published_matches(target: &ReplacementTarget<'_>, stage: &ReplacementStage) -> io::Result<bool> {
    if !target.parent_matches_registration()?
        || stage.directory.identity()?.volume != target.source_snapshot.volume
        || stage.directory.exists(OsStr::new(STAGE_NAME))?
    {
        return Ok(false);
    }
    let Some(published) = target.observe_target()? else {
        return Ok(false);
    };
    let snapshot = match file_snapshot(&published) {
        Ok(snapshot) => snapshot,
        Err(error) => return observed_false(error),
    };
    Ok(has_exact_name(&published, &target.name)?
        && stage_metadata(&published)? == stage.metadata
        && file_bytes_matches(&published, &stage.proposed)?
        && file_snapshot(&published)? == snapshot
        && stage_metadata(&published)? == stage.metadata)
}

/// Replace `name` beneath `parent` with the file behind `stage`, by handle.
#[allow(unsafe_code)]
fn rename_onto(stage: &File, parent: &File, name: &OsStr) -> io::Result<()> {
    let units = encode_component(name)?;
    if units.is_empty() {
        return Err(invalid_input("replacement requires a target name"));
    }
    let name_bytes = units
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| invalid_input("replacement name is too long"))?;
    let name_offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
    // Windows refuses a buffer smaller than the structure with a length mismatch.
    let total = name_offset
        .checked_add(name_bytes)
        .ok_or_else(|| invalid_input("replacement name is too long"))?
        .max(size_of::<FILE_RENAME_INFORMATION>());
    let length = u32::try_from(total).map_err(|_| invalid_input("rename buffer is too large"))?;
    let file_name_length =
        u32::try_from(name_bytes).map_err(|_| invalid_input("replacement name is too long"))?;
    let mut storage = vec![0u64; total.div_ceil(size_of::<u64>())];
    let base = storage.as_mut_ptr().cast::<u8>();
    let information = base.cast::<FILE_RENAME_INFORMATION>();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `storage` is zeroed, aligned for the structure, and holds at least
    // `total` bytes, so every field and all of `name_bytes` at `name_offset` are
    // in bounds. `parent` outlives the synchronous call that reads its handle.
    let status = unsafe {
        (*information).Anonymous.Flags =
            FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
        (*information).RootDirectory = parent.as_raw_handle() as HANDLE;
        (*information).FileNameLength = file_name_length;
        std::ptr::copy_nonoverlapping(
            units.as_ptr().cast::<u8>(),
            base.add(name_offset),
            name_bytes,
        );
        NtSetInformationFile(
            stage.as_raw_handle() as HANDLE,
            &mut status_block,
            information.cast(),
            length,
            FileRenameInformationEx,
        )
    };
    if status != 0 {
        return Err(native_error(status));
    }
    Ok(())
}

fn file_snapshot(file: &File) -> io::Result<FileSnapshot> {
    let identity = query_identity(file)?;
    validate_regular_file(file, identity.VolumeSerialNumber)?;
    let basic = query_basic(file)?;
    Ok(FileSnapshot {
        volume: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
        length: query_standard(file)?.EndOfFile,
        last_write: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        attributes: query_attributes(file)?.FileAttributes,
    })
}

fn snapshot_matches(file: &File, expected: FileSnapshot) -> io::Result<bool> {
    match file_snapshot(file) {
        Ok(actual) => Ok(actual == expected),
        Err(error) => observed_false(error),
    }
}

fn metadata_is_supported(file: &File) -> io::Result<bool> {
    match validate_file_metadata(file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(false),
        Err(error) => Err(error),
    }
}

fn security_matches(file: &File, expected: &NativeSecurity) -> io::Result<bool> {
    match read_native_security(file) {
        Ok(security) => Ok(security == *expected),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(false),
        Err(error) => Err(error),
    }
}

fn file_bytes_matches(file: &File, expected: &[u8]) -> io::Result<bool> {
    let length =
        usize::try_from(query_standard(file)?.EndOfFile).map_err(|_| length_out_of_range())?;
    Ok(length == expected.len() && read_from_start(file, length)? == expected)
}

/// Read from offset zero without disturbing the caller's bound.
fn read_from_start(file: &File, limit: usize) -> io::Result<Vec<u8>> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    read_bounded(reader, limit)
}

/// Refuse a case or short-name alias, which replacement would silently rewrite.
fn require_exact_name(file: &File, expected: &OsStr) -> io::Result<()> {
    if !has_exact_name(file, expected)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement requires the exact stored file name",
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn has_exact_name(file: &File, expected: &OsStr) -> io::Result<bool> {
    let mut storage = vec![0u64; NAME_BUFFER_BYTES / size_of::<u64>()];
    let length =
        u32::try_from(NAME_BUFFER_BYTES).map_err(|_| invalid_data("name buffer is too large"))?;
    // SAFETY: `file` is live and `storage` is aligned and writable for `length`.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileNormalizedNameInfo,
            storage.as_mut_ptr().cast(),
            length,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the buffer is fully initialized and this view does not outlive it.
    let bytes =
        unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), NAME_BUFFER_BYTES) };
    Ok(final_component(bytes)?
        .iter()
        .copied()
        .eq(expected.encode_wide()))
}

/// Parse a `FILE_NAME_INFO` record and return its last path component.
fn final_component(bytes: &[u8]) -> io::Result<Vec<u16>> {
    let field = bytes
        .get(..size_of::<u32>())
        .and_then(|field| <[u8; 4]>::try_from(field).ok())
        .ok_or_else(|| invalid_data("native name record is truncated"))?;
    let start = offset_of!(FILE_NAME_INFO, FileName);
    let end = usize::try_from(u32::from_le_bytes(field))
        .ok()
        .and_then(|length| start.checked_add(length))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid_data("native name exceeds its bounds"))?;
    let units: Vec<u16> = bytes[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Ok(units
        .rsplit(|unit| *unit == 92)
        .next()
        .unwrap_or(&[])
        .to_vec())
}

fn validate_same_volume(expected: u64, target: u64, storage: u64) -> io::Result<()> {
    if target != expected || storage != expected {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement storage and target are not on the same volume",
        ));
    }
    Ok(())
}

/// Treat an object that no longer validates as a change, and anything else as
/// a failure the caller must report.
fn observed_false(error: io::Error) -> io::Result<bool> {
    if is_observed_change(&error) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn is_observed_change(error: &io::Error) -> bool {
    const CHANGED: [NTSTATUS; 6] = [
        STATUS_OBJECT_NAME_NOT_FOUND,
        STATUS_FILE_IS_A_DIRECTORY,
        STATUS_NOT_A_DIRECTORY,
        STATUS_REPARSE_POINT_ENCOUNTERED,
        STATUS_DELETE_PENDING,
        STATUS_SHARING_VIOLATION,
    ];
    error.kind() == io::ErrorKind::InvalidData
        || error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<NativeStatus>())
            .is_some_and(|status| CHANGED.contains(&status.0))
}

fn changed_during_operation() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "file changed during read")
}

fn length_out_of_range() -> io::Error {
    invalid_data("native file length cannot be represented")
}

#[cfg(test)]
mod tests;
