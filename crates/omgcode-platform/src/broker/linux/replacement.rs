//! Linux ext4-only handle-relative replacement publication.

use super::{
    FileSnapshot, NativeWorkspace, RelativePath, changed_during_operation, invalid_data,
    invalid_input, regular_single_link_snapshot, validate_checkpoint_metadata,
};
use crate::broker::{FileCapture, MAX_FILE_BYTES};
use crate::directory_identity;
use crate::private_store::{NativeSecurity, PrivateDirectory, PrivateFileIdentity, StageMetadata};
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, ResolveFlags, StatxFlags, fchmod, fchown, fsync, openat2,
    renameat_with, statx,
};
use rustix::process::{Gid, Uid};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::SystemTime;

const MOUNT_INFO_LIMIT: usize = 1024 * 1024;
const MOUNT_INFO_READ_LIMIT: u64 = 1024 * 1024 + 1;
const STAGE_NAME: &str = "stage";

/// A retained existing workspace entry and its bounded expected version.
pub struct ReplacementTarget<'a> {
    workspace: &'a NativeWorkspace,
    path: RelativePath,
    parent: File,
    name: OsString,
    source: File,
    source_snapshot: FileSnapshot,
    parent_created: SystemTime,
    capture: FileCapture,
    mount_id: u64,
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
}

impl NativeWorkspace {
    pub(in crate::broker) fn replacement_target<'a>(
        &'a self,
        path: &RelativePath,
        max_bytes: usize,
    ) -> io::Result<ReplacementTarget<'a>> {
        let workspace = self.workspace_handle()?;
        let (parent, name) = self.parent_and_name(&workspace, path)?;
        let mount_id = directory_mount_id(&parent)?;
        require_ext4_mount(mount_id)?;
        let pinned = openat2(
            &parent,
            Path::new(&name),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH
                | ResolveFlags::NO_SYMLINKS
                | ResolveFlags::NO_MAGICLINKS
                | ResolveFlags::NO_XDEV,
        )?;
        let pinned = File::from(pinned);
        let initial_snapshot = super::regular_single_link_snapshot(&pinned)?;
        let source = super::reopen_pinned_file(&pinned)?;
        let source_snapshot = super::regular_single_link_snapshot(&source)?;
        if !initial_snapshot.same_object_and_version(source_snapshot) {
            return Err(changed_during_operation());
        }
        let max_bytes_u64 = u64::try_from(max_bytes)
            .map_err(|_| invalid_input("replacement capture limit cannot be represented"))?;
        if source_snapshot.length > max_bytes_u64 {
            return Err(invalid_data("replacement source exceeds the capture limit"));
        }
        validate_checkpoint_metadata(&source, source_snapshot)?;
        crate::private_store::linux::validate_replacement_attributes(&source)?;
        if source_snapshot.owner != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "replacement requires a source owned by the effective user",
            ));
        }
        let source_length = usize::try_from(source_snapshot.length)
            .map_err(|_| invalid_data("replacement source length cannot be represented"))?;
        let bytes = read_file(&source, source_length)?;
        let after = regular_single_link_snapshot(&source)?;
        if bytes.len() != source_length
            || !source_snapshot.same_object_and_version(after)
            || !metadata_is_supported(&source, after)?
        {
            return Err(changed_during_operation());
        }
        let parent_created = parent.metadata()?.created()?;
        Ok(ReplacementTarget {
            workspace: self,
            path: path.clone(),
            parent,
            name,
            source,
            source_snapshot,
            parent_created,
            capture: FileCapture {
                bytes,
                volume: source_snapshot.device,
                file_id: u128::from(source_snapshot.inode).to_le_bytes(),
                security: NativeSecurity::Unix {
                    owner: source_snapshot.owner,
                    group: source_snapshot.group,
                    mode: source_snapshot.mode & 0o777,
                },
            },
            mount_id,
            #[cfg(test)]
            hook: None,
        })
    }
}

impl ReplacementTarget<'_> {
    /// Captured bytes and metadata from the pinned source object.
    pub fn capture(&self) -> &FileCapture {
        &self.capture
    }

    /// Check the target mount and ext4 qualification before private artifacts.
    pub fn check_storage(&self, directory: &PrivateDirectory) -> io::Result<()> {
        directory.identity()?;
        let target_mount = directory_mount_id(&self.parent)?;
        let storage_mount = directory_mount_id(directory.native_file())?;
        validate_same_mount(self.mount_id, target_mount, storage_mount)?;
        // Probe the exact retained target parent before any operation artifacts.
        fsync(&self.parent)?;
        Ok(())
    }

    /// Exclusively create and durably prepare one bounded private stage file.
    pub fn prepare_stage(
        &self,
        directory: PrivateDirectory,
        proposed: &[u8],
    ) -> io::Result<ReplacementStage> {
        if proposed.len() > MAX_FILE_BYTES {
            return Err(invalid_input("replacement proposal exceeds 1 MiB"));
        }
        self.check_storage(&directory)?;
        let mut file = directory.create_file(OsStr::new(STAGE_NAME))?;
        file.write_all(proposed)?;
        fchown(
            &file,
            Some(Uid::from_raw(self.source_snapshot.owner)),
            Some(Gid::from_raw(self.source_snapshot.group)),
        )?;
        fchmod(
            &file,
            Mode::from_raw_mode(self.source_snapshot.mode & 0o777),
        )?;
        file.sync_all()?;
        directory.sync()?;
        let snapshot = regular_single_link_snapshot(&file)?;
        let metadata = crate::private_store::stage_metadata(&file)?;
        if metadata.security != self.capture.security || !file_bytes_matches(&file, proposed)? {
            return Err(invalid_data(
                "replacement stage metadata or bytes do not match",
            ));
        }
        if crate::private_store::stage_metadata(&file)? != metadata
            || !snapshot.same_object_and_version(regular_single_link_snapshot(&file)?)
        {
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
        if !self
            .source_snapshot
            .same_object_and_version(stage.source_snapshot)
            || directory_mount_id(stage.directory.native_file())? != self.mount_id
        {
            return Err(invalid_input(
                "stage does not belong to the retained source and mount",
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
        let current = match openat2(
            &self.parent,
            Path::new(&self.name),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH
                | ResolveFlags::NO_SYMLINKS
                | ResolveFlags::NO_MAGICLINKS
                | ResolveFlags::NO_XDEV,
        ) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::XDEV) => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let snapshot = match super::regular_single_link_snapshot(&current) {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Ok(false),
            Err(error) => return Err(error),
        };
        let readable = super::reopen_pinned_file(&current)?;
        if !snapshot.same_object_and_version(super::regular_single_link_snapshot(&readable)?) {
            return Ok(false);
        }
        if !self.source_snapshot.same_object_and_version(snapshot)
            || !metadata_is_supported(&readable, snapshot)?
            || !file_bytes_matches(&readable, &self.capture.bytes)?
            || !snapshot_matches(&readable, snapshot)?
            || !metadata_is_supported(&readable, snapshot)?
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn parent_matches_registration(&self) -> io::Result<bool> {
        let workspace = match self.workspace.workspace_handle() {
            Ok(workspace) => workspace,
            Err(error) if is_observed_path_change(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let (parent, _) = match self.workspace.parent_and_name(&workspace, &self.path) {
            Ok(result) => result,
            Err(error) if is_observed_path_change(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let identity = directory_identity(&parent)?;
        let created = parent.metadata()?.created()?;
        Ok(identity == directory_identity(&self.parent)?
            && created == self.parent_created
            && directory_mount_id(&parent)? == self.mount_id)
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
        before_attempt()?;
        #[cfg(test)]
        self.run_hook(TestPoint::Rename)?;
        renameat_with(
            stage.directory.native_file(),
            OsStr::new(STAGE_NAME),
            &self.parent,
            &self.name,
            RenameFlags::empty(),
        )?;
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
        fsync(&self.parent)?;
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
        Ok(crate::private_store::stage_metadata(&self.file)?.identity)
    }
}

fn validate_stage(stage: &ReplacementStage) -> io::Result<()> {
    let opened = stage.directory.open_stage()?;
    let initial = regular_single_link_snapshot(&opened)?;
    let metadata = crate::private_store::stage_metadata(&opened)?;
    if metadata != stage.metadata {
        return Err(invalid_data("replacement stage metadata changed"));
    }
    if !file_bytes_matches(&opened, &stage.proposed)? {
        return Err(invalid_data("replacement stage bytes changed"));
    }
    let after = regular_single_link_snapshot(&opened)?;
    if !initial.same_object_and_version(after)
        || crate::private_store::stage_metadata(&opened)? != stage.metadata
    {
        return Err(invalid_data(
            "replacement stage changed during verification",
        ));
    }
    Ok(())
}

fn published_matches(target: &ReplacementTarget<'_>, stage: &ReplacementStage) -> io::Result<bool> {
    if !target.parent_matches_registration()?
        || directory_mount_id(stage.directory.native_file())? != target.mount_id
        || stage.directory.exists(OsStr::new(STAGE_NAME))?
    {
        return Ok(false);
    }
    let published = openat2(
        &target.parent,
        Path::new(&target.name),
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH
            | ResolveFlags::NO_SYMLINKS
            | ResolveFlags::NO_MAGICLINKS
            | ResolveFlags::NO_XDEV,
    )?;
    let published = File::from(published);
    let snapshot = match super::regular_single_link_snapshot(&published) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Ok(false),
        Err(error) => return Err(error),
    };
    let readable = super::reopen_pinned_file(&published)?;
    if !snapshot.same_object_and_version(super::regular_single_link_snapshot(&readable)?) {
        return Ok(false);
    }
    let metadata = crate::private_store::stage_metadata(&readable)?;
    if metadata != stage.metadata {
        return Ok(false);
    }
    if !file_bytes_matches(&readable, &stage.proposed)? {
        return Ok(false);
    }
    let after = regular_single_link_snapshot(&readable)?;
    Ok(snapshot.same_object_and_version(after)
        && crate::private_store::stage_metadata(&readable)? == stage.metadata)
}

fn file_bytes_matches(file: &File, expected: &[u8]) -> io::Result<bool> {
    Ok(read_file(file, expected.len())? == expected)
}

fn read_file(file: &File, limit: usize) -> io::Result<Vec<u8>> {
    let mut reopened = file.try_clone()?;
    use std::io::{Seek, SeekFrom};
    reopened.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(limit);
    let read_limit = u64::try_from(limit)
        .ok()
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| invalid_input("replacement byte bound cannot be represented"))?;
    Read::by_ref(&mut reopened)
        .take(read_limit)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn snapshot_matches(file: &File, expected: FileSnapshot) -> io::Result<bool> {
    match regular_single_link_snapshot(file) {
        Ok(actual) => Ok(expected.same_object_and_version(actual)),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(false),
        Err(error) => Err(error),
    }
}

fn metadata_is_supported(file: &File, snapshot: FileSnapshot) -> io::Result<bool> {
    match super::validate_checkpoint_metadata(file, snapshot) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    }
    match crate::private_store::linux::validate_replacement_attributes(file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(false),
        Err(error) => Err(error),
    }
}

fn directory_mount_id(directory: &File) -> io::Result<u64> {
    let status = statx(directory, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    let id = mount_id_from_statx(status.stx_mask, status.stx_mnt_id)?;
    if id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux returned an invalid zero mount ID",
        ));
    }
    Ok(id)
}

fn mount_id_from_statx(mask: u32, id: u64) -> io::Result<u64> {
    if mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Linux STATX_MNT_ID is unavailable",
        ));
    }
    Ok(id)
}

fn validate_same_mount(expected: u64, target: u64, storage: u64) -> io::Result<()> {
    if target != expected || storage != expected {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement storage and target are not on the same mounted filesystem",
        ));
    }
    Ok(())
}

fn require_ext4_mount(mount_id: u64) -> io::Result<()> {
    let mut file = File::open("/proc/self/mountinfo")?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MOUNT_INFO_READ_LIMIT)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MOUNT_INFO_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "mountinfo exceeds the replacement bound",
        ));
    }
    let expected = mount_id.to_string();
    let filesystem = mount_type_for_id(&bytes, expected.as_bytes())?;
    if filesystem != b"ext4" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "replacement requires ext4",
        ));
    }
    Ok(())
}

fn mount_type_for_id<'a>(bytes: &'a [u8], expected: &[u8]) -> io::Result<&'a [u8]> {
    let mut matches = 0usize;
    let mut filesystem = None;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut fields = line
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty());
        let id = fields.next();
        let parent_id = fields.next();
        let device = fields.next();
        let root = fields.next();
        let mount_point = fields.next();
        let mount_options = fields.next();
        if id != Some(expected) {
            continue;
        }
        matches += 1;
        if !parent_id.is_some_and(decimal)
            || !device.is_some_and(device_number)
            || root.is_none()
            || mount_point.is_none()
            || mount_options.is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "matching mountinfo record has invalid fixed fields",
            ));
        }
        let mut separator_count = 0usize;
        let mut this_filesystem = None;
        while let Some(field) = fields.next() {
            if field == b"-" {
                separator_count += 1;
                this_filesystem = fields.next();
                if this_filesystem.is_none() || fields.next().is_none() || fields.next().is_none() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "matching mountinfo record has incomplete filesystem fields",
                    ));
                }
            }
        }
        if separator_count != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "matching mountinfo record has invalid separator count",
            ));
        }
        filesystem = this_filesystem;
    }
    if matches != 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "mount ID has no unique mountinfo record",
        ));
    }
    filesystem.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "matching mountinfo record has no filesystem type",
        )
    })
}

fn decimal(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

fn device_number(bytes: &[u8]) -> bool {
    let mut numbers = bytes.split(|byte| *byte == b':');
    numbers
        .next()
        .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        && numbers
            .next()
            .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        && numbers.next().is_none()
}

fn is_observed_path_change(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidData
        || error.raw_os_error().is_some_and(|code| {
            code == libc::ENOENT
                || code == libc::ENOTDIR
                || code == libc::ELOOP
                || code == libc::EXDEV
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::sync::{Arc, Mutex};

    fn fixture() -> (tempfile::TempDir, NativeWorkspace, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("workspace root");
        let workspace_path = root.path().join("workspace");
        fs::create_dir(&workspace_path).expect("workspace");
        let workspace = NativeWorkspace::open(
            root.path(),
            directory_identity(&File::open(root.path()).expect("root handle"))
                .expect("root identity"),
            &RelativePath::new(Path::new("workspace")).expect("workspace path"),
            directory_identity(&File::open(&workspace_path).expect("workspace handle"))
                .expect("workspace identity"),
        )
        .expect("native workspace");
        let storage = tempfile::Builder::new()
            .prefix("replacement-test-")
            .tempdir()
            .expect("private storage");
        fs::set_permissions(storage.path(), fs::Permissions::from_mode(0o700))
            .expect("private mode");
        (root, workspace, storage)
    }

    fn target<'a>(workspace: &'a NativeWorkspace) -> ReplacementTarget<'a> {
        workspace
            .replacement_target(
                &RelativePath::new(Path::new("file")).expect("target path"),
                1024,
            )
            .expect("target capture")
    }

    #[test]
    fn publication_replaces_name_once_and_preserves_supported_metadata() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"before").expect("source bytes");
        fs::set_permissions(&source_path, fs::Permissions::from_mode(0o640)).expect("source mode");
        let target = target(&workspace);
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        target
            .check_storage(&directory)
            .expect("same ext4 mount preflight");
        let old_id = PrivateFileIdentity {
            volume: target.capture.volume,
            file_id: target.capture.file_id,
        };
        let mut stage = target.prepare_stage(directory, b"after").expect("stage");
        assert!(target.recheck(&stage).expect("precommit checks"));
        target.commit(&mut stage, || Ok(())).expect("publish");
        assert_eq!(fs::read(&source_path).expect("published bytes"), b"after");
        assert_ne!(stage.identity().expect("stage identity"), old_id);
        let published = fs::metadata(&source_path).expect("published metadata");
        assert_eq!(
            target.capture.security,
            NativeSecurity::Unix {
                owner: published.uid(),
                group: published.gid(),
                mode: published.mode() & 0o777,
            }
        );
        assert!(!storage.path().join("stage").exists());
        assert!(
            target.commit(&mut stage, || Ok(())).is_err(),
            "second commit attempt is refused"
        );
    }

    #[test]
    fn a_stage_cannot_publish_to_a_different_retained_source() {
        let (root, workspace, storage) = fixture();
        let first_path = root.path().join("workspace/file");
        let second_path = root.path().join("workspace/other");
        fs::write(&first_path, b"same bytes").expect("first source");
        fs::write(&second_path, b"same bytes").expect("second source");
        let first = target(&workspace);
        let second = workspace
            .replacement_target(
                &RelativePath::new(Path::new("other")).expect("second target path"),
                1024,
            )
            .expect("second target capture");
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = first
            .prepare_stage(directory, b"proposed")
            .expect("stage for first source");

        assert!(second.commit(&mut stage, || Ok(())).is_err());
        assert_eq!(
            fs::read(&first_path).expect("first preserved"),
            b"same bytes"
        );
        assert_eq!(
            fs::read(&second_path).expect("second preserved"),
            b"same bytes"
        );
        assert_eq!(
            fs::read(storage.path().join("stage")).expect("stage retained"),
            b"proposed"
        );
    }

    /// Use two real mounts to verify that storage is refused before a stage exists.
    #[test]
    fn private_storage_on_a_different_mount_cannot_stage() {
        let (root, workspace, _storage) = fixture();
        let source = root.path().join("workspace/file");
        fs::write(&source, b"before").expect("source bytes");
        let target = target(&workspace);
        let other = tempfile::Builder::new()
            .prefix("omgcode-cross-mount-")
            .tempdir_in("/dev/shm")
            .expect("disposable tmpfs directory");
        fs::set_permissions(other.path(), fs::Permissions::from_mode(0o700)).expect("private mode");
        let path = fs::canonicalize(other.path()).expect("native fixture path");
        let directory = PrivateDirectory::open(&path).expect("existing private directory");
        let other_id = directory_mount_id(directory.native_file()).expect("actual mount ID");
        assert_ne!(target.mount_id, other_id);
        assert_eq!(
            require_ext4_mount(other_id)
                .expect_err("tmpfs unavailable")
                .kind(),
            io::ErrorKind::Unsupported
        );
        let refusal = target
            .prepare_stage(directory, b"after")
            .err()
            .expect("cross-mount refusal");
        assert_eq!(refusal.kind(), io::ErrorKind::Unsupported);
        assert!(!path.join("stage").exists());
        assert_eq!(fs::read(source).expect("unchanged source"), b"before");
    }

    /// Move the test stage at the last boundary, then observe a real rename error.
    #[test]
    fn kernel_rename_error_does_not_trigger_a_second_attempt() {
        let (root, workspace, storage) = fixture();
        let source = root.path().join("workspace/file");
        fs::write(&source, b"before").expect("source bytes");
        let mut target = target(&workspace);
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = target
            .prepare_stage(directory, b"after")
            .expect("stage bytes");
        let stage_path = storage.path().join("stage");
        let retained = storage.path().join("retained-by-test");
        let held_path = retained.clone();
        target.hook = Some(Arc::new(move |point| {
            if point == TestPoint::AfterFinalCheck {
                fs::rename(&stage_path, &held_path)?;
            }
            Ok(())
        }));
        let first = target
            .commit(&mut stage, || Ok(()))
            .expect_err("kernel cannot find old name");
        assert_eq!(first.raw_os_error(), Some(libc::ENOENT));
        let second = target
            .commit(&mut stage, || Ok(()))
            .expect_err("attempt consumed");
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(source).expect("unchanged target"), b"before");
        assert_eq!(
            fs::read(retained).expect("stage remains with independent writer"),
            b"after"
        );
    }

    #[test]
    fn recheck_rejects_same_bytes_with_a_different_inode_and_content_changes() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"same").expect("source");
        let target = target(&workspace);
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let stage = target.prepare_stage(directory, b"new").expect("stage");
        fs::remove_file(&source_path).expect("remove fixture source");
        fs::write(&source_path, b"same").expect("equal bytes with new inode");
        assert!(!target.recheck(&stage).expect("identity conflict"));
        fs::write(&source_path, b"changed").expect("changed source");
        assert!(!target.recheck(&stage).expect("content conflict"));
    }

    #[test]
    fn commit_hook_records_order_and_failed_attempt_cannot_repeat() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"before").expect("source");
        let mut target = target(&workspace);
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&events);
        target.hook = Some(Arc::new(move |point| {
            recorded.lock().expect("event lock").push(point);
            Ok(())
        }));
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = target.prepare_stage(directory, b"after").expect("stage");
        target.commit(&mut stage, || Ok(())).expect("commit");
        assert_eq!(
            *events.lock().expect("events"),
            vec![
                TestPoint::AfterFinalCheck,
                TestPoint::Rename,
                TestPoint::PublishedCheck,
                TestPoint::StageSync,
                TestPoint::SourceParentSync,
                TestPoint::TargetParentSync,
            ]
        );
        assert!(target.commit(&mut stage, || Ok(())).is_err());
    }

    #[test]
    fn rename_fault_is_one_uncertain_attempt_and_keeps_stage() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"before").expect("source");
        let mut target = target(&workspace);
        target.hook = Some(Arc::new(|point| {
            if point == TestPoint::Rename {
                return Err(io::Error::other("injected rename fault"));
            }
            Ok(())
        }));
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = target.prepare_stage(directory, b"after").expect("stage");
        assert!(target.commit(&mut stage, || Ok(())).is_err());
        assert!(
            target.commit(&mut stage, || Ok(())).is_err(),
            "the attempted stage is not retryable"
        );
        assert_eq!(fs::read(source_path).expect("unchanged target"), b"before");
        assert!(
            storage.path().join("stage").exists(),
            "stage evidence remains"
        );
    }

    #[test]
    fn last_moment_authorization_refusal_preserves_target_and_stage() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"before").expect("source");
        let target = target(&workspace);
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = target.prepare_stage(directory, b"after").expect("stage");
        let error = target
            .commit(&mut stage, || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "authority expired",
                ))
            })
            .expect_err("expired authority must refuse before rename");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&source_path).expect("unchanged target"), b"before");
        assert!(
            storage.path().join(STAGE_NAME).exists(),
            "stage evidence remains"
        );
        assert!(
            target.commit(&mut stage, || Ok(())).is_err(),
            "a refused attempt cannot be retried"
        );
    }

    #[test]
    fn post_rename_sync_failures_remain_uncertain_and_preserve_visible_state() {
        for fault in [
            TestPoint::PublishedCheck,
            TestPoint::StageSync,
            TestPoint::SourceParentSync,
            TestPoint::TargetParentSync,
        ] {
            let (root, workspace, storage) = fixture();
            let source_path = root.path().join("workspace/file");
            fs::write(&source_path, b"before").expect("source");
            let mut target = target(&workspace);
            target.hook = Some(Arc::new(move |point| {
                if point == fault {
                    return Err(io::Error::other("injected post-rename failure"));
                }
                Ok(())
            }));
            let directory = PrivateDirectory::open(storage.path()).expect("private directory");
            let mut stage = target.prepare_stage(directory, b"after").expect("stage");
            assert!(
                target.commit(&mut stage, || Ok(())).is_err(),
                "{fault:?} is uncertain"
            );
            assert_eq!(fs::read(&source_path).expect("published target"), b"after");
            assert!(
                !storage.path().join("stage").exists(),
                "renamed stage evidence is at target"
            );
            assert!(
                target.commit(&mut stage, || Ok(())).is_err(),
                "{fault:?} is not retryable"
            );
        }
    }

    #[test]
    fn change_after_final_check_can_be_overwritten_and_is_not_claimed_as_cas() {
        let (root, workspace, storage) = fixture();
        let source_path = root.path().join("workspace/file");
        fs::write(&source_path, b"captured").expect("source");
        let mut target = target(&workspace);
        let changed_path = source_path.clone();
        target.hook = Some(Arc::new(move |point| {
            if point == TestPoint::AfterFinalCheck {
                fs::write(&changed_path, b"independent edit").expect("independent edit");
            }
            Ok(())
        }));
        let directory = PrivateDirectory::open(storage.path()).expect("private directory");
        let mut stage = target.prepare_stage(directory, b"proposal").expect("stage");
        target
            .commit(&mut stage, || Ok(()))
            .expect("rename can overwrite later independent edit");
        assert_eq!(fs::read(source_path).expect("published bytes"), b"proposal");
        assert_eq!(
            target.capture().bytes,
            b"captured",
            "the checkpoint input stays separate"
        );
    }

    #[test]
    fn symlink_source_and_xattr_source_are_refused() {
        let (root, workspace, _storage) = fixture();
        let workspace_path = root.path().join("workspace");
        fs::write(root.path().join("elsewhere"), b"source").expect("outside source");
        symlink(root.path().join("elsewhere"), workspace_path.join("file")).expect("symlink");
        assert!(
            workspace
                .replacement_target(&RelativePath::new(Path::new("file")).expect("path"), 1024)
                .is_err()
        );
        fs::remove_file(workspace_path.join("file")).expect("remove xattr source");
        fs::write(workspace_path.join("file"), b"special mode").expect("special mode source");
        fs::set_permissions(
            workspace_path.join("file"),
            fs::Permissions::from_mode(0o4755),
        )
        .expect("set special mode");
        assert!(
            workspace
                .replacement_target(&RelativePath::new(Path::new("file")).expect("path"), 1024)
                .is_err()
        );
        fs::remove_file(workspace_path.join("file")).expect("remove symlink");
        fs::write(workspace_path.join("file"), b"source").expect("ordinary source");
        rustix::fs::setxattr(
            workspace_path.join("file"),
            "user.test",
            b"x",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("xattr fixture");
        assert!(
            workspace
                .replacement_target(&RelativePath::new(Path::new("file")).expect("path"), 1024)
                .is_err()
        );
    }

    #[test]
    fn private_open_stage_refuses_a_symlink_name() {
        let root = tempfile::Builder::new()
            .prefix("stage-link-")
            .tempdir()
            .expect("private root");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private mode");
        fs::write(root.path().join("payload"), b"payload").expect("payload");
        symlink(root.path().join("payload"), root.path().join(STAGE_NAME)).expect("stage symlink");
        let directory = PrivateDirectory::open(root.path()).expect("private directory");
        assert!(directory.open_stage().is_err());
    }

    #[test]
    fn mountinfo_parser_requires_one_exact_id_and_type_after_separator() {
        let records = b"26 1 0:1 / /proc rw - proc proc rw\n42 1 8:1 / / rw - ext4 /dev/root rw\n";
        assert_eq!(
            mount_type_for_id(records, b"42").expect("ext4 record"),
            b"ext4"
        );
        assert!(mount_type_for_id(records, b"26").is_ok());
        assert_ne!(
            mount_type_for_id(records, b"26").expect("proc record"),
            b"ext4"
        );
        assert!(mount_type_for_id(records, b"9").is_err());
        assert!(mount_type_for_id(b"42 1 8:1 / / rw ext4 /dev/root rw\n", b"42").is_err());
        assert!(
            mount_type_for_id(
                b"42 1 8:1 / / rw - ext4 /dev/a rw\n42 1 8:2 / /x rw - ext4 /dev/b rw\n",
                b"42"
            )
            .is_err()
        );
        assert!(validate_same_mount(7, 7, 8).is_err());
        assert!(validate_same_mount(7, 6, 7).is_err());
        assert!(validate_same_mount(7, 7, 7).is_ok());
        assert!(mount_id_from_statx(0, 0).is_err());
        assert_eq!(
            mount_id_from_statx(StatxFlags::MNT_ID.bits(), 42).expect("present ID"),
            42
        );
    }
}
