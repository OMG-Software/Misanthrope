//! Landlock filesystem ruleset for a confined run.
//!
//! The parent detects the ABI, creates the ruleset, and adds one
//! path-beneath rule per confinement path, each from a descriptor opened
//! without following any symbolic link. The child only calls
//! `landlock_restrict_self` on the prepared descriptor, so nothing allocates
//! between `fork` and `exec`. Rules bind to the opened objects, so renaming a
//! path after preparation does not change what is granted.
//!
//! ABI 3 is the minimum: earlier versions do not handle truncation, so a
//! read-only file could still be truncated by path. Rights that later ABIs add
//! are handled when the kernel reports them; rights added after ABI 6 are not.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use rustix::fs::{CWD, FileType, Mode, OFlags, ResolveFlags};

use crate::process::{Confinement, ProcessError, UnixSocketAccess};

/// The oldest ABI accepted. See the module documentation.
pub(super) const MINIMUM_ABI: u32 = 3;

const CREATE_RULESET_VERSION: libc::c_uint = 1 << 0;
const RULE_PATH_BENEATH: libc::c_int = 1;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
/// Every right from `EXECUTE` (bit 0) to `MAKE_SYM` (bit 12), all in ABI 1.
const ACCESS_FS_ABI_1: u64 = (1 << 13) - 1;
const ACCESS_FS_REFER: u64 = 1 << 13;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;
/// The rights that apply to a non-directory rule.
const ACCESS_FS_FILE: u64 = ACCESS_FS_EXECUTE
    | ACCESS_FS_WRITE_FILE
    | ACCESS_FS_READ_FILE
    | ACCESS_FS_TRUNCATE
    | ACCESS_FS_IOCTL_DEV;
const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;
const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;

/// `struct landlock_ruleset_attr`. The size passed to the kernel grows with
/// the ABI, so an older kernel never sees a field it does not know.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// `struct landlock_path_beneath_attr`, which the kernel declares packed.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// The rights and attribute size used for one ABI version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RulesetPlan {
    pub(super) abi: u32,
    pub(super) handled_fs: u64,
    pub(super) handled_net: u64,
    pub(super) scoped: u64,
    pub(super) attr_size: usize,
}

impl RulesetPlan {
    /// Read, list, and execute.
    pub(super) fn read_only_access(self) -> u64 {
        (ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR) & self.handled_fs
    }

    /// Every handled right except creating device nodes, and creating socket
    /// files unless host Unix sockets are allowed.
    pub(super) fn read_write_access(self, unix_sockets: UnixSocketAccess) -> u64 {
        let mut withheld = ACCESS_FS_MAKE_CHAR | ACCESS_FS_MAKE_BLOCK;
        if unix_sockets == UnixSocketAccess::PairsOnly {
            withheld |= ACCESS_FS_MAKE_SOCK;
        }
        self.handled_fs & !withheld
    }
}

/// Choose what to handle for a kernel that reports `abi`. Fails closed below
/// [`MINIMUM_ABI`].
pub(super) fn plan(abi: u32) -> Result<RulesetPlan, ProcessError> {
    if abi < MINIMUM_ABI {
        return Err(ProcessError::Unsupported(
            "confined execution needs Landlock ABI 3 or later",
        ));
    }
    let mut handled_fs = ACCESS_FS_ABI_1 | ACCESS_FS_REFER | ACCESS_FS_TRUNCATE;
    if abi >= 5 {
        handled_fs |= ACCESS_FS_IOCTL_DEV;
    }
    let (handled_net, net_size) = if abi >= 4 {
        (ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP, 16)
    } else {
        (0, 8)
    };
    let (scoped, attr_size) = if abi >= 6 {
        (SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL, 24)
    } else {
        (0, net_size)
    };
    Ok(RulesetPlan {
        abi,
        handled_fs,
        handled_net,
        scoped,
        attr_size,
    })
}

/// A prepared ruleset. Dropping it closes the parent's descriptor.
pub(super) struct Ruleset {
    descriptor: OwnedFd,
    abi: u32,
}

impl Ruleset {
    /// Detect the ABI, create the ruleset, and add every confinement path.
    pub(super) fn prepare(confinement: &Confinement) -> Result<Self, ProcessError> {
        let plan = plan(abi_version()?)?;
        let descriptor = create(&plan)?;
        for path in &confinement.read_only {
            add_path(&descriptor, path, plan.read_only_access())?;
        }
        let read_write = plan.read_write_access(confinement.unix_sockets);
        for path in &confinement.read_write {
            add_path(&descriptor, path, read_write)?;
        }
        Ok(Self {
            descriptor,
            abi: plan.abi,
        })
    }

    pub(super) fn raw_fd(&self) -> RawFd {
        self.descriptor.as_raw_fd()
    }

    pub(super) fn abi(&self) -> u32 {
        self.abi
    }
}

#[allow(unsafe_code)]
fn abi_version() -> Result<u32, ProcessError> {
    // SAFETY: with the version flag, a null attribute pointer and zero size
    // are the documented query form; the kernel reads no memory.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOSYS | libc::EOPNOTSUPP) => Err(ProcessError::Unsupported(
                "Landlock is not available in this kernel",
            )),
            _ => Err(ProcessError::Confinement(error)),
        };
    }
    u32::try_from(result).map_err(|_| ProcessError::Unsupported("Landlock ABI is out of range"))
}

#[allow(unsafe_code)]
fn create(plan: &RulesetPlan) -> Result<OwnedFd, ProcessError> {
    let attr = RulesetAttr {
        handled_access_fs: plan.handled_fs,
        handled_access_net: plan.handled_net,
        scoped: plan.scoped,
    };
    // SAFETY: `attr` is a live, initialized `landlock_ruleset_attr`, and
    // `attr_size` never exceeds its size. The kernel only reads it.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            plan.attr_size.min(size_of::<RulesetAttr>()),
            0 as libc::c_uint,
        )
    };
    if result < 0 {
        return Err(ProcessError::Confinement(io::Error::last_os_error()));
    }
    let raw = RawFd::try_from(result)
        .map_err(|_| ProcessError::Confinement(io::Error::other("invalid ruleset descriptor")))?;
    // SAFETY: the kernel returned a new close-on-exec descriptor that nothing
    // else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Open `path` without following any symbolic link, check its type, and add
/// it with the rights that apply to that type.
#[allow(unsafe_code)]
fn add_path(ruleset: &OwnedFd, path: &Path, access: u64) -> Result<(), ProcessError> {
    let object = rustix::fs::openat2(
        CWD,
        path,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(|error| ProcessError::Confinement(error.into()))?;
    let status =
        rustix::fs::fstat(&object).map_err(|error| ProcessError::Confinement(error.into()))?;
    let allowed = match FileType::from_raw_mode(status.st_mode) {
        FileType::Directory => access,
        FileType::RegularFile | FileType::CharacterDevice => access & ACCESS_FS_FILE,
        _ => {
            return Err(ProcessError::Confinement(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a confinement path must be a directory, regular file, or character device",
            )));
        }
    };
    let attr = PathBeneathAttr {
        allowed_access: allowed,
        parent_fd: object.as_raw_fd(),
    };
    // SAFETY: `attr` is a live, initialized `landlock_path_beneath_attr`; the
    // ruleset and the opened object stay open for the call. The kernel only
    // reads the attribute.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            &raw const attr,
            0 as libc::c_uint,
        )
    };
    if result != 0 {
        return Err(ProcessError::Confinement(io::Error::last_os_error()));
    }
    Ok(())
}

/// Enforce the ruleset on the calling thread and its future children. Runs in
/// the forked child after `no_new_privs`; it allocates nothing.
#[allow(unsafe_code)]
pub(super) fn restrict_self(ruleset: RawFd) -> io::Result<()> {
    // SAFETY: integer arguments only; the descriptor was inherited from the
    // parent's live ruleset, and no memory is passed.
    let result =
        unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0 as libc::c_uint) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abis_before_three_are_refused() {
        for abi in 0..MINIMUM_ABI {
            assert!(
                matches!(plan(abi), Err(ProcessError::Unsupported(_))),
                "{abi}"
            );
        }
    }

    #[test]
    fn abi_three_handles_truncation_without_network_or_scope() {
        let plan = plan(3).expect("ABI 3");
        assert_eq!(plan.handled_fs & ACCESS_FS_TRUNCATE, ACCESS_FS_TRUNCATE);
        assert_eq!(plan.handled_fs & ACCESS_FS_REFER, ACCESS_FS_REFER);
        assert_eq!(plan.handled_fs & ACCESS_FS_IOCTL_DEV, 0);
        assert_eq!((plan.handled_net, plan.scoped, plan.attr_size), (0, 0, 8));
    }

    #[test]
    fn later_abis_add_tcp_device_and_scope_rules() {
        let four = plan(4).expect("ABI 4");
        assert_eq!(
            four.handled_net,
            ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP
        );
        assert_eq!((four.scoped, four.attr_size), (0, 16));
        let five = plan(5).expect("ABI 5");
        assert_eq!(five.handled_fs & ACCESS_FS_IOCTL_DEV, ACCESS_FS_IOCTL_DEV);
        for abi in [6, 7, 42] {
            let plan = plan(abi).expect("ABI 6 or later");
            assert_eq!(plan.scoped, SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL);
            assert_eq!(plan.attr_size, size_of::<RulesetAttr>());
        }
    }

    #[test]
    fn read_only_rules_never_grant_changes() {
        for abi in 3..=7 {
            let access = plan(abi).expect("plan").read_only_access();
            assert_eq!(
                access,
                ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR
            );
        }
    }

    #[test]
    fn read_write_rules_withhold_devices_and_sockets_by_default() {
        let plan = plan(6).expect("plan");
        let pairs = plan.read_write_access(UnixSocketAccess::PairsOnly);
        assert_eq!(pairs & (ACCESS_FS_MAKE_CHAR | ACCESS_FS_MAKE_BLOCK), 0);
        assert_eq!(pairs & ACCESS_FS_MAKE_SOCK, 0);
        assert_eq!(pairs & ACCESS_FS_TRUNCATE, ACCESS_FS_TRUNCATE);
        let host = plan.read_write_access(UnixSocketAccess::HostServices);
        assert_eq!(host & ACCESS_FS_MAKE_SOCK, ACCESS_FS_MAKE_SOCK);
        assert_eq!(host & (ACCESS_FS_MAKE_CHAR | ACCESS_FS_MAKE_BLOCK), 0);
    }
}
