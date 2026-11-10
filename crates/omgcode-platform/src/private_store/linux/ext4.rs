//! Read-only ext4 inode attributes that statx does not report.

use std::{fs::File, io, mem::size_of};

use rustix::fs::ioctl_getflags;
use rustix::ioctl::{self, Opcode, Updater, opcode};

// Linux UAPI FS_EXTENT_FL is an allocation representation, not a permission.
// Do not silently preserve or discard any other visible inode flag.
const EXTENTS: u32 = 0x0008_0000;

// Linux include/uapi/linux/fs.h, struct fsxattr. All fields have valid values
// for every bit pattern, and the reserved bytes are part of the output buffer.
#[repr(C)]
#[derive(Default)]
struct FilesystemAttributes {
    flags: u32,
    extent_size: u32,
    extent_count: u32,
    project: u32,
    cow_extent_size: u32,
    reserved: [u8; 8],
}

const _: () = assert!(size_of::<FilesystemAttributes>() == 28);
const FSGETXATTR: Opcode = opcode::read::<FilesystemAttributes>(b'X', 31);

/// Reject unpreserved inode flags, project IDs, and allocation hints on the
/// same regular file. A missing or failed metadata query prevents publication.
pub(super) fn validate_file_attributes(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    if ioctl_getflags(file)?.bits() & !EXTENTS != 0 {
        return Err(unsupported_attributes());
    }
    validate_attributes(read_attributes(file)?)
}

fn validate_attributes(attributes: FilesystemAttributes) -> io::Result<()> {
    if attributes.flags != 0
        || attributes.extent_size != 0
        || attributes.project != 0
        || attributes.cow_extent_size != 0
        || attributes.reserved != [0; 8]
    {
        return Err(unsupported_attributes());
    }
    // The kernel-managed extent count, like the inode number, can change.
    Ok(())
}

// Rustix exposes safe GETFLAGS, but no safe FSGETXATTR wrapper. Keep the
// read-only fixed-layout query here; never pass an input path or raw FD onward.
#[allow(unsafe_code)]
fn read_attributes(file: &File) -> io::Result<FilesystemAttributes> {
    let mut attributes = FilesystemAttributes::default();
    // SAFETY: FS_IOC_FSGETXATTR is _IOR('X', 31, struct fsxattr). The C-layout
    // buffer above has exactly that size and alignment and starts initialized.
    // Updater borrows it exclusively through the synchronous ioctl. The kernel
    // only writes the fixed output buffer and retains no pointer. The borrowed
    // File is the live regular-file handle checked by validate_file_attributes.
    unsafe {
        ioctl::ioctl(file, Updater::<FSGETXATTR, _>::new(&mut attributes))?;
    }
    Ok(attributes)
}

fn unsupported_attributes() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "replacement cannot preserve these inode attributes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{IFlags, ioctl_setflags};

    #[test]
    fn ext4_attributes_refuse_noatime_even_when_statx_omits_it() {
        let file = tempfile::tempfile().expect("disposable ext4 file");
        validate_file_attributes(&file).expect("ordinary ext4 attributes");
        let original = ioctl_getflags(&file).expect("original inode flags");
        let changed = original | IFlags::NOATIME;
        ioctl_setflags(&file, changed).expect("set disposable file's noatime flag");
        assert_eq!(
            validate_file_attributes(&file)
                .expect_err("noatime cannot be preserved")
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(ioctl_getflags(&file).expect("retained flags"), changed);
    }

    #[test]
    fn filesystem_attributes_allow_allocation_count_but_refuse_project_metadata() {
        validate_attributes(FilesystemAttributes {
            extent_count: 2,
            ..Default::default()
        })
        .expect("allocation count can differ on the new inode");
        let attributed = FilesystemAttributes {
            project: 7,
            ..Default::default()
        };
        assert_eq!(
            validate_attributes(attributed)
                .expect_err("project identity is unsupported")
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
