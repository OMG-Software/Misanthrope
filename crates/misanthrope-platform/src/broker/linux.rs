//! Linux handle-relative workspace operations.
//!
//! `openat2` is required. This module does not fall back to ambient path
//! operations when the kernel, seccomp policy, or filesystem rejects a flag.
//! Directory handles anchor each operation, but a concurrent hostile process
//! with the same user identity can still rename an opened directory elsewhere.

use super::{CreateError, DirectoryEntry, EntryKind, Listing, RelativePath};
use crate::{DirectoryIdentity, directory_identity};
use rustix::fd::AsFd;
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, fsync, mkdirat, open, openat2, statat,
};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const OPEN_RETRIES: usize = 4;
fn root_resolve() -> ResolveFlags {
    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS
}

fn workspace_resolve() -> ResolveFlags {
    root_resolve() | ResolveFlags::NO_XDEV
}

fn file_mode() -> Mode {
    Mode::RUSR | Mode::WUSR
}

fn directory_mode() -> Mode {
    Mode::RUSR | Mode::WUSR | Mode::XUSR
}

/// An opened root and workspace. Handles are owned here and dropped with it.
pub(super) struct NativeWorkspace {
    root_path: PathBuf,
    workspace_relative: PathBuf,
    root: File,
    workspace: File,
    root_created: SystemTime,
    workspace_created: SystemTime,
}

impl NativeWorkspace {
    /// Open the configured root and workspace without following links.
    ///
    /// The root path is resolved from `/` and may cross mount points to reach
    /// the selected root. Workspace traversal then forbids links and mount
    /// crossings. No path-based fallback is permitted.
    pub(super) fn open(
        root: &Path,
        expected_root: DirectoryIdentity,
        workspace_relative: &RelativePath,
        expected_workspace: DirectoryIdentity,
    ) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(invalid_input("development root must be absolute"));
        }

        let root_handle = open_root(root)?;
        let root_metadata = validate_directory(&root_handle, expected_root, None)?;
        let workspace_relative = path_or_dot(workspace_relative);
        let workspace_handle = open_confined(
            &root_handle,
            &workspace_relative,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )?;
        let workspace_metadata = validate_directory(&workspace_handle, expected_workspace, None)?;

        Ok(Self {
            root_path: root.to_path_buf(),
            workspace_relative,
            root: root_handle,
            workspace: workspace_handle,
            root_created: root_metadata.created()?,
            workspace_created: workspace_metadata.created()?,
        })
    }

    /// Return the recorded directory creation times after checking live names.
    pub(super) fn creation_times(&self) -> io::Result<(SystemTime, SystemTime)> {
        self.workspace_handle()?;
        Ok((self.root_created, self.workspace_created))
    }

    /// Read one regular file with one hard link and a strict byte limit.
    pub(super) fn read(&self, path: &RelativePath, max_bytes: usize) -> io::Result<Vec<u8>> {
        if path.is_empty() || max_bytes == 0 || max_bytes > super::MAX_FILE_BYTES {
            return Err(invalid_input("invalid bounded file read"));
        }

        let workspace = self.workspace_handle()?;
        let relative = path.to_path_buf();
        let object = open_confined(
            &workspace,
            &relative,
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )?;
        let initial = regular_single_link_snapshot(&object)?;
        if initial.length > max_bytes as u64 {
            return Err(invalid_data("file exceeds the read limit"));
        }

        // This is a generated descriptor path, not caller-controlled input.
        // It reopens the exact inode held by `object`, after type and link-count
        // checks, and fails closed if procfs is not available.
        let mut readable = reopen_pinned_file(&object)?;
        let reopened = regular_single_link_snapshot(&readable)?;
        if !initial.same_object_and_version(reopened) {
            return Err(changed_during_operation());
        }

        let take_limit = u64::try_from(max_bytes)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or_else(|| invalid_input("file read limit is too large"))?;
        let initial_length = usize::try_from(initial.length)
            .map_err(|_| invalid_data("file length does not fit this platform"))?;
        let mut bytes = Vec::with_capacity(initial_length);
        Read::by_ref(&mut readable)
            .take(take_limit)
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_bytes {
            return Err(invalid_data("file grew beyond the read limit"));
        }
        let after = regular_single_link_snapshot(&readable)?;
        if !reopened.same_object_and_version(after) {
            return Err(changed_during_operation());
        }
        Ok(bytes)
    }

    /// Enumerate native names from an opened directory with bounded output.
    pub(super) fn list(
        &self,
        path: &RelativePath,
        max_entries: usize,
        max_name_units: usize,
    ) -> io::Result<Listing> {
        if !(1..=super::MAX_DIRECTORY_ENTRIES).contains(&max_entries)
            || !(1..=super::MAX_DIRECTORY_NAME_UNITS).contains(&max_name_units)
        {
            return Err(invalid_input("invalid directory listing limits"));
        }

        let workspace = self.workspace_handle()?;
        let directory = open_confined(
            &workspace,
            &path_or_dot(path),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )?;
        let mut iterator = Dir::read_from(&directory)?;
        let mut entries = Vec::new();
        let mut name_units = 0usize;
        let mut truncated = false;

        while let Some(entry) = iterator.read() {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if entries.len() == max_entries
                || name.len() > max_name_units.saturating_sub(name_units)
            {
                truncated = true;
                break;
            }

            let native_name = OsStr::from_bytes(name).to_os_string();
            let kind = classify_entry(&directory, &native_name)?;
            name_units = name_units
                .checked_add(name.len())
                .ok_or_else(|| invalid_data("directory name byte count overflowed"))?;
            entries.push(DirectoryEntry {
                name: native_name,
                kind,
            });
        }

        Ok(Listing { entries, truncated })
    }

    /// Exclusively create a regular file and sync its data and parent entry.
    ///
    /// If any operation fails after `openat2` creates the entry, the entry is
    /// left in place and `may_have_created` is true. It is never unlinked here.
    pub(super) fn create_new(&self, path: &RelativePath, data: &[u8]) -> Result<(), CreateError> {
        if path.is_empty() || data.len() > super::MAX_FILE_BYTES {
            return Err(not_created(invalid_input("invalid new-file request")));
        }

        let workspace = self.workspace_handle().map_err(not_created)?;
        let (parent, name) = self
            .parent_and_name(&workspace, path)
            .map_err(not_created)?;
        let descriptor = open_confined(
            &parent,
            Path::new(&name),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            file_mode(),
            workspace_resolve(),
        )
        .map_err(not_created)?;
        let mut file = descriptor;

        if let Err(error) = file.write_all(data) {
            return Err(may_have_created(error));
        }
        if let Err(error) = file.sync_all() {
            return Err(may_have_created(error));
        }
        if let Err(error) = fsync(&parent) {
            return Err(may_have_created(error.into()));
        }
        Ok(())
    }

    /// Exclusively create one directory below an already existing parent.
    pub(super) fn create_dir(&self, path: &RelativePath) -> Result<(), CreateError> {
        if path.is_empty() {
            return Err(not_created(invalid_input("invalid directory request")));
        }

        let workspace = self.workspace_handle().map_err(not_created)?;
        let (parent, name) = self
            .parent_and_name(&workspace, path)
            .map_err(not_created)?;
        if let Err(error) = mkdirat(&parent, &name, directory_mode()) {
            return Err(not_created(error.into()));
        }

        let created = open_confined(
            &parent,
            Path::new(&name),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )
        .map_err(may_have_created)?;
        if let Err(error) = validate_created_directory(&created) {
            return Err(may_have_created(error));
        }
        if let Err(error) = fsync(&parent) {
            return Err(may_have_created(error.into()));
        }
        Ok(())
    }

    fn workspace_handle(&self) -> io::Result<File> {
        let root = open_root(&self.root_path)?;
        let root_identity = directory_identity(&self.root)?;
        validate_directory(&root, root_identity, Some(self.root_created))?;
        let workspace = open_confined(
            &root,
            &self.workspace_relative,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )?;
        let workspace_identity = directory_identity(&self.workspace)?;
        validate_directory(&workspace, workspace_identity, Some(self.workspace_created))?;
        Ok(workspace)
    }

    fn parent_and_name(
        &self,
        workspace: &File,
        path: &RelativePath,
    ) -> io::Result<(File, OsString)> {
        let Some(name) = path.components.last() else {
            return Err(invalid_input("operation requires a relative entry name"));
        };
        let mut parent_path = PathBuf::new();
        for component in path.components.iter().take(path.components.len() - 1) {
            parent_path.push(component);
        }
        let parent = open_confined(
            workspace,
            &path_or_dot_buf(parent_path),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            workspace_resolve(),
        )?;
        Ok((parent, name.clone()))
    }
}

fn open_root(root: &Path) -> io::Result<File> {
    let slash = open(
        "/",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let relative = root
        .strip_prefix("/")
        .map_err(|_| invalid_input("development root must be absolute"))?;
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    open_confined(
        &slash,
        relative,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        root_resolve(),
    )
}

fn open_confined(
    directory: &impl AsFd,
    path: &Path,
    flags: OFlags,
    mode: Mode,
    resolve: ResolveFlags,
) -> io::Result<File> {
    for attempt in 0..OPEN_RETRIES {
        match openat2(directory, path, flags, mode, resolve) {
            Ok(descriptor) => return Ok(File::from(descriptor)),
            Err(error) if is_errno(error, Errno::AGAIN) && attempt + 1 < OPEN_RETRIES => continue,
            Err(error) if is_errno(error, Errno::NOSYS) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Linux openat2 is required for workspace access",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "workspace path changed during repeated resolution",
    ))
}

fn validate_directory(
    directory: &File,
    expected: DirectoryIdentity,
    expected_created: Option<SystemTime>,
) -> io::Result<std::fs::Metadata> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || directory_identity(directory)? != expected {
        return Err(changed_directory());
    }
    let created = metadata.created()?;
    if expected_created.is_some_and(|expected| expected != created) {
        return Err(changed_directory());
    }
    Ok(metadata)
}

fn validate_created_directory(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(invalid_data("created entry is not a directory"));
    }
    directory_identity(directory)?;
    metadata.created()?;
    Ok(())
}

/// Reopen only the exact inode already checked through its held O_PATH handle.
fn reopen_pinned_file(object: &File) -> io::Result<File> {
    let pinned_path = PathBuf::from(format!("/proc/self/fd/{}", object.as_raw_fd()));
    File::open(pinned_path).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ) {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "procfs descriptor reopening is required for workspace reads",
            )
        } else {
            error
        }
    })
}

fn classify_entry(directory: &File, name: &OsStr) -> io::Result<EntryKind> {
    let metadata = statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let kind = FileType::from_raw_mode(metadata.st_mode);
    Ok(match kind {
        FileType::Directory => EntryKind::Directory,
        FileType::RegularFile if metadata.st_nlink == 1 => EntryKind::File,
        _ => EntryKind::Blocked,
    })
}

#[derive(Clone, Copy)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}

impl FileSnapshot {
    fn same_object_and_version(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.links == other.links
            && self.length == other.length
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanos == other.modified_nanos
            && self.changed_seconds == other.changed_seconds
            && self.changed_nanos == other.changed_nanos
    }
}

fn regular_single_link_snapshot(file: &File) -> io::Result<FileSnapshot> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(invalid_input(
            "workspace reads require an ordinary file with one hard link",
        ));
    }
    Ok(FileSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        links: metadata.nlink(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
    })
}

fn path_or_dot(path: &RelativePath) -> PathBuf {
    path_or_dot_buf(path.to_path_buf())
}

fn path_or_dot_buf(path: PathBuf) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

fn is_errno(error: Errno, expected: Errno) -> bool {
    error == expected
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn changed_directory() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "workspace directory changed")
}

fn changed_during_operation() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "file changed during read")
}

fn not_created(error: io::Error) -> CreateError {
    CreateError {
        error,
        may_have_created: false,
    }
}

fn may_have_created(error: io::Error) -> CreateError {
    CreateError {
        error,
        may_have_created: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn relative(path: &str) -> RelativePath {
        RelativePath::new(Path::new(path)).expect("valid relative path")
    }

    fn workspace() -> (tempfile::TempDir, NativeWorkspace) {
        let temporary = tempfile::tempdir().expect("temporary root");
        let root = temporary.path().join("root");
        let selected = root.join("workspace");
        fs::create_dir(&root).expect("create root");
        fs::create_dir(&selected).expect("create workspace");
        let root_handle = File::open(&root).expect("open root for fixture identity");
        let workspace_handle = File::open(&selected).expect("open workspace for fixture identity");
        let native = NativeWorkspace::open(
            &root,
            directory_identity(&root_handle).expect("root identity"),
            &relative("workspace"),
            directory_identity(&workspace_handle).expect("workspace identity"),
        )
        .expect("open native workspace");
        (temporary, native)
    }

    #[test]
    fn reads_lists_and_exclusively_creates_bounded_entries() {
        let (temporary, workspace) = workspace();
        workspace
            .create_new(&relative("file.txt"), b"hello")
            .expect("create file");
        workspace
            .create_dir(&relative("child"))
            .expect("create directory");

        assert_eq!(
            workspace.read(&relative("file.txt"), 5).expect("read file"),
            b"hello"
        );
        assert!(workspace.read(&relative("file.txt"), 4).is_err());
        let listing = workspace
            .list(&relative(""), 10, 100)
            .expect("list workspace");
        assert!(!listing.truncated);
        assert!(listing.entries.iter().any(|entry| {
            entry.name == OsStr::new("file.txt") && entry.kind == EntryKind::File
        }));
        assert!(listing.entries.iter().any(|entry| {
            entry.name == OsStr::new("child") && entry.kind == EntryKind::Directory
        }));

        let disk = fs::read(temporary.path().join("root/workspace/file.txt"))
            .expect("read created disk file");
        assert_eq!(disk, b"hello");
    }

    #[test]
    fn create_collision_preserves_the_existing_file() {
        let (temporary, workspace) = workspace();
        let path = temporary.path().join("root/workspace/file.txt");
        fs::write(&path, b"original").expect("write original file");

        let error = workspace
            .create_new(&relative("file.txt"), b"replacement")
            .expect_err("create_new must refuse an existing name");

        assert!(!error.may_have_created);
        assert_eq!(fs::read(path).expect("read original"), b"original");
    }

    #[test]
    fn links_hardlinks_and_fifos_are_blocked_without_blocking_reads() {
        let (temporary, workspace) = workspace();
        let selected = temporary.path().join("root/workspace");
        fs::write(selected.join("target"), b"content").expect("write target");
        fs::hard_link(selected.join("target"), selected.join("hardlink"))
            .expect("create hard link");
        std::os::unix::fs::symlink(selected.join("target"), selected.join("symlink"))
            .expect("create symlink");
        let opened_workspace = workspace.workspace_handle().expect("workspace handle");
        rustix::fs::mkfifoat(&opened_workspace, "fifo", Mode::RUSR | Mode::WUSR)
            .expect("create fifo");

        assert!(workspace.read(&relative("hardlink"), 10).is_err());
        assert!(workspace.read(&relative("symlink"), 10).is_err());
        assert!(workspace.read(&relative("fifo"), 10).is_err());
        let listing = workspace
            .list(&relative(""), 10, 100)
            .expect("list entries");
        for blocked in ["hardlink", "symlink", "fifo"] {
            assert!(listing.entries.iter().any(|entry| {
                entry.name == OsStr::new(blocked) && entry.kind == EntryKind::Blocked
            }));
        }
    }

    #[test]
    fn replacement_or_move_of_workspace_path_is_rejected() {
        let (temporary, workspace) = workspace();
        let root = temporary.path().join("root");
        let old = root.join("workspace");
        fs::rename(&old, root.join("moved")).expect("move saved workspace");
        fs::create_dir(&old).expect("replace saved path");

        let error = workspace
            .list(&relative(""), 10, 100)
            .expect_err("reject replacement");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn listing_stops_at_entry_and_name_bounds() {
        let (_temporary, workspace) = workspace();
        workspace
            .create_new(&relative("one"), b"1")
            .expect("create first file");
        workspace
            .create_new(&relative("two"), b"2")
            .expect("create second file");

        let by_count = workspace
            .list(&relative(""), 1, 100)
            .expect("bounded listing");
        assert_eq!(by_count.entries.len(), 1);
        assert!(by_count.truncated);
        let by_name = workspace
            .list(&relative(""), 10, 3)
            .expect("bounded name units");
        assert_eq!(by_name.entries.len(), 1);
        assert!(by_name.truncated);
    }
}
