use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rustix::fs::{AtFlags, Dir, Mode, OFlags, ResolveFlags, mkdirat, open, openat2, statat};
use rustix::io::Errno;

use crate::{DirectoryIdentity, directory_identity};

pub(super) fn file_identity(file: &File) -> io::Result<super::PrivateFileIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || !private_mode(&metadata, 0o600) {
        return Err(invalid(
            "private file type, links, ownership, or mode is invalid",
        ));
    }
    Ok(super::PrivateFileIdentity {
        volume: metadata.dev(),
        file_id: u128::from(metadata.ino()).to_le_bytes(),
    })
}

/// A retained private directory. Owned handles close on drop.
///
/// The caller must compare the root identity with its trusted registration and
/// own any required writer lock. Same-user hostile mutation is not isolated.
pub struct PrivateDirectory {
    file: File,
}

impl PrivateDirectory {
    /// Open an existing absolute root without traversing any symbolic link.
    /// Mounts are allowed while selecting this trusted root. Children refuse
    /// mount crossings. The root must belong to the effective UID with mode0700.
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("private root must be absolute"));
        }
        let slash = open(
            "/",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let relative = path
            .strip_prefix("/")
            .map_err(|_| invalid("private root must be absolute"))?;
        let relative = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        };
        let file = native_open(
            &slash,
            relative,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            root_flags(),
        )?;
        let directory = Self { file };
        directory.validate()?;
        Ok(directory)
    }

    /// Return the opened directory identity after rechecking privacy.
    pub fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.validate()?;
        directory_identity(&self.file)
    }
    /// Return native creation time. Unavailable metadata is an error.
    pub fn created(&self) -> io::Result<SystemTime> {
        self.validate()?.created()
    }

    /// Open one existing private child directory. No ambient reopen is used.
    pub fn open_dir(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        let parent = self.validate()?;
        let file = native_open(
            &self.file,
            Path::new(name),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            child_flags(),
        )?;
        let directory = Self { file };
        if directory.validate()?.dev() != parent.dev() {
            return Err(invalid("private directory changed filesystem"));
        }
        Ok(directory)
    }

    /// Exclusively create one private directory and sync its parent entry.
    /// Any error may leave an entry. Nothing is retried or deleted.
    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        self.validate()?;
        mkdirat(&self.file, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
        let directory = self.open_dir(name)?;
        self.sync()?;
        Ok(directory)
    }

    /// Open a pinned regular single-link private file. Ownership and mode0600
    /// are checked before procfs reopens its exact inode. The caller owns the
    /// returned file and must bound reads/writes and retain any required lock.
    pub fn open_file(&self, name: &OsStr, writable: bool) -> io::Result<File> {
        self.open_file_with(name, writable, || Ok(()))
    }

    fn open_file_with(
        &self,
        name: &OsStr,
        writable: bool,
        after_pin: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<File> {
        validate_name(name)?;
        let parent = self.validate()?;
        let pinned = native_open(
            &self.file,
            Path::new(name),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            child_flags(),
        )?;
        let initial = validate_file(&pinned, parent.dev())?;
        after_pin()?;
        // Only this retained descriptor supplies the generated procfs path.
        // The process feature supplies a safe effective-UID check, not execution.
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", pinned.as_raw_fd()));
        let reopened = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(descriptor_path)?;
        let checked = validate_file(&reopened, parent.dev())?;
        if initial.dev() != checked.dev() || initial.ino() != checked.ino() {
            return Err(invalid("private file identity changed during reopen"));
        }
        Ok(reopened)
    }

    /// Exclusively create a private regular file and sync its parent entry.
    /// The returned file is empty; the caller must write and sync its contents.
    /// Any error may leave an entry. There is no retry or cleanup.
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        validate_name(name)?;
        let parent = self.validate()?;
        let file = native_open(
            &self.file,
            Path::new(name),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
            child_flags(),
        )?;
        validate_file(&file, parent.dev())?;
        self.sync()?;
        Ok(file)
    }

    /// Observe whether one name exists without following its final link.
    /// This observation does not validate or authorize a later open.
    pub fn exists(&self, name: &OsStr) -> io::Result<bool> {
        validate_name(name)?;
        self.validate()?;
        match statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Enumerate at most limit (1..=128) native names. Overflow is an error,
    /// never a successful truncated inventory. No child object is followed.
    pub fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        if !(1..=128).contains(&limit) {
            return Err(invalid("invalid private inventory bound"));
        }
        self.validate()?;
        let mut iterator = Dir::read_from(&self.file)?;
        let mut names = Vec::new();
        while let Some(entry) = iterator.read() {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if names.len() == limit || bytes.len() > 255 {
                return Err(invalid("private inventory exceeds bound"));
            }
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
        Ok(names)
    }

    /// Synchronize this actual directory. Unsupported sync is an error.
    pub fn sync(&self) -> io::Result<()> {
        self.validate()?;
        self.file.sync_all()
    }

    fn validate(&self) -> io::Result<Metadata> {
        let metadata = self.file.metadata()?;
        if !metadata.is_dir() || !private_mode(&metadata, 0o700) {
            return Err(invalid("private directory ownership or mode is invalid"));
        }
        Ok(metadata)
    }
}

fn private_mode(metadata: &Metadata, mode: u32) -> bool {
    // Safe rustix process metadata is used only to read the current effective UID.
    metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o7777 == mode
}

fn validate_file(file: &File, device: u64) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.dev() != device
        || !private_mode(&metadata, 0o600)
    {
        return Err(invalid(
            "private file type, links, ownership, mode, or filesystem is invalid",
        ));
    }
    Ok(metadata)
}

fn validate_name(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private operation requires one native entry name",
        ));
    }
    Ok(())
}

fn root_flags() -> ResolveFlags {
    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS
}
fn child_flags() -> ResolveFlags {
    root_flags() | ResolveFlags::NO_XDEV
}
fn native_open(
    parent: &impl rustix::fd::AsFd,
    path: &Path,
    flags: OFlags,
    mode: Mode,
    resolve: ResolveFlags,
) -> io::Result<File> {
    match openat2(parent, path, flags, mode, resolve) {
        Ok(descriptor) => Ok(File::from(descriptor)),
        Err(Errno::NOSYS | Errno::INVAL) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private storage requires Linux openat2",
        )),
        Err(error) => Err(error.into()),
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::Builder::new()
            .prefix("private-store-test-")
            .tempdir()
            .expect("fixture");
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
            .expect("private fixture root");
        root
    }

    fn private_file(path: &Path, contents: &[u8]) {
        fs::write(path, contents).expect("fixture file");
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private fixture");
    }

    #[test]
    fn private_entries_round_trip_with_exclusive_creation_and_real_sync() {
        let root = private_root();
        let directory = PrivateDirectory::open(root.path()).expect("private root");
        assert_eq!(
            directory.identity().expect("identity"),
            directory_identity(&File::open(root.path()).expect("root handle"))
                .expect("expected identity")
        );
        directory.created().expect("native creation metadata");
        assert!(!directory.exists(OsStr::new("entry")).expect("absence"));
        let child = directory
            .create_dir(OsStr::new("entry"))
            .expect("exclusive directory");
        let mut file = child
            .create_file(OsStr::new("original"))
            .expect("exclusive file");
        file.write_all(b"exact checkpoint bytes").expect("payload");
        file.sync_all().expect("payload sync");
        child.sync().expect("parent sync");
        assert!(child.create_file(OsStr::new("original")).is_err());
        assert!(directory.create_dir(OsStr::new("entry")).is_err());
        let mut bytes = Vec::new();
        child
            .open_file(OsStr::new("original"), false)
            .expect("private reopen")
            .read_to_end(&mut bytes)
            .expect("read");
        assert_eq!(bytes, b"exact checkpoint bytes");
        assert_eq!(
            directory.names(1).expect("one name"),
            vec![OsString::from("entry")]
        );
        assert_eq!(
            fs::metadata(root.path().join("entry"))
                .expect("directory mode")
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.path().join("entry/original"))
                .expect("file mode")
                .mode()
                & 0o7777,
            0o600
        );
    }

    #[test]
    fn non_private_modes_and_symlinked_roots_or_children_are_refused() {
        let parent = private_root();
        let root = parent.path().join("private");
        fs::create_dir(&root).expect("directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("broad mode");
        assert!(PrivateDirectory::open(&root).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private mode");
        let directory = PrivateDirectory::open(&root).expect("private root");
        let alias = parent.path().join("alias");
        symlink(&root, &alias).expect("root symlink");
        assert!(PrivateDirectory::open(&alias).is_err());
        symlink(parent.path(), root.join("escape")).expect("child symlink");
        assert!(directory.open_dir(OsStr::new("escape")).is_err());
        assert!(directory.open_file(OsStr::new("escape"), false).is_err());
        private_file(&root.join("file"), b"private");
        fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o644))
            .expect("broad file mode");
        assert!(directory.open_file(OsStr::new("file"), false).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("widen retained root");
        assert!(
            directory
                .create_file(OsStr::new("must-not-create"))
                .is_err()
        );
        assert!(!root.join("must-not-create").exists());
    }

    #[test]
    fn linked_and_special_files_never_supply_data() {
        let root = private_root();
        let directory = PrivateDirectory::open(root.path()).expect("private root");
        private_file(&root.path().join("file"), b"private");
        fs::hard_link(root.path().join("file"), root.path().join("hard")).expect("hard link");
        assert!(directory.open_file(OsStr::new("file"), false).is_err());
        symlink("file", root.path().join("link")).expect("symlink");
        assert!(directory.open_file(OsStr::new("link"), false).is_err());
        rustix::fs::mkfifoat(&directory.file, "fifo", Mode::RUSR | Mode::WUSR).expect("FIFO");
        assert!(directory.open_file(OsStr::new("fifo"), false).is_err());
        assert!(
            directory
                .exists(OsStr::new("link"))
                .expect("nofollow existence observation")
        );
    }

    #[test]
    fn pinned_reopen_does_not_read_a_substituted_path() {
        let root = private_root();
        let directory = PrivateDirectory::open(root.path()).expect("private root");
        private_file(&root.path().join("file"), b"captured");
        let mut file = directory
            .open_file_with(OsStr::new("file"), false, || {
                fs::rename(root.path().join("file"), root.path().join("saved"))?;
                private_file(&root.path().join("file"), b"substitution");
                Ok(())
            })
            .expect("pinned inode");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read pinned");
        assert_eq!(bytes, b"captured");
        assert_eq!(
            fs::read(root.path().join("file")).expect("replacement"),
            b"substitution"
        );
    }

    #[test]
    fn private_file_identity_uses_the_same_handle_and_distinguishes_files() {
        let root = private_root();
        let directory = PrivateDirectory::open(root.path()).expect("private root");
        let first = directory
            .create_file(OsStr::new("first"))
            .expect("first file");
        let second = directory
            .create_file(OsStr::new("second"))
            .expect("second file");
        let identity = super::file_identity(&first).expect("first identity");
        assert_eq!(
            identity,
            super::file_identity(&first).expect("stable identity")
        );
        assert_ne!(
            identity,
            super::file_identity(&second).expect("distinct identity")
        );
        assert_eq!(
            identity.volume,
            first.metadata().expect("same handle metadata").dev()
        );
        assert_eq!(
            identity.file_id,
            u128::from(first.metadata().expect("same handle metadata").ino()).to_le_bytes()
        );
        assert!(super::file_identity(&directory.file).is_err());
        fs::hard_link(root.path().join("first"), root.path().join("alias")).expect("hard link");
        assert!(super::file_identity(&first).is_err());
    }

    #[test]
    fn invalid_names_and_inventory_overflow_fail_instead_of_truncating() {
        let root = private_root();
        let directory = PrivateDirectory::open(root.path()).expect("private root");
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "nested/name",
            "/absolute",
            "nul\0",
        ] {
            assert!(directory.create_file(OsStr::new(name)).is_err(), "{name:?}");
            assert!(directory.open_dir(OsStr::new(name)).is_err());
        }
        directory.create_file(OsStr::new("one")).expect("first");
        directory.create_file(OsStr::new("two")).expect("second");
        assert!(directory.names(1).is_err());
        assert!(directory.names(0).is_err());
        assert!(directory.names(129).is_err());
        assert_eq!(
            directory
                .names(2)
                .expect("complete bounded inventory")
                .len(),
            2
        );
    }
}
