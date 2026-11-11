//! Filesystem and network confinement requested for one run.
//!
//! The rules are an allowlist: a confined program can reach only the listed
//! paths, cannot create IP sockets, follows the selected Unix-domain socket
//! mode, and passes the same restrictions to every descendant. Linux
//! applies them with Landlock and seccomp; Windows refuses confined runs until
//! the AppContainer increment exists. See `docs/execution.md` and ADR-0028.

use std::path::{Path, PathBuf};

use super::{ProcessError, contains_nul};

/// Largest number of paths, read-only and read-write together, in one confinement.
pub const MAX_CONFINEMENT_PATHS: usize = 64;

/// Filesystem allowlist for a confined run. Everything not listed is denied.
///
/// Each path must be absolute and name an existing directory, regular file,
/// or character device. On Linux no component of the path may be a symbolic
/// link, so a rule always names the object the caller checked. A directory
/// rule covers everything beneath it. Rules are additive: a read-only entry
/// inside a read-write directory is still writable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Confinement {
    /// Paths that may be read and executed but never changed. Toolchain roots,
    /// system libraries, and the program itself belong here.
    pub read_only: Vec<PathBuf>,
    /// Paths that may also be written, created, renamed, and removed beneath.
    /// The workspace and a private scratch directory belong here.
    pub read_write: Vec<PathBuf>,
    /// Which Unix-domain sockets the tree may create. Defaults to pairs only.
    pub unix_sockets: UnixSocketAccess,
}

/// Which Unix-domain sockets a confined tree may create. IP and every other
/// address family stay denied in both modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnixSocketAccess {
    /// Only `socketpair` of stream or sequenced-packet type, for IPC inside
    /// the tree. No socket can connect to a host service. This is the default.
    #[default]
    PairsOnly,
    /// Also `socket(AF_UNIX)` of any type, and creating socket files in
    /// read-write paths. The tree can then connect to **every pathname
    /// socket the user can reach**, including the Docker daemon, the D-Bus
    /// session bus, and SSH agents; Landlock cannot limit which ones.
    /// Connecting to the Docker daemon is usually equivalent to root on the
    /// host. Select this only on an explicit operator decision.
    HostServices,
}

impl Confinement {
    /// Check the shape of every rule before anything is opened or started.
    pub(super) fn validate(&self) -> Result<(), ProcessError> {
        let count = self.read_only.len().saturating_add(self.read_write.len());
        if count > MAX_CONFINEMENT_PATHS {
            return Err(ProcessError::InvalidSpec("too many confinement paths"));
        }
        for path in self.read_only.iter().chain(&self.read_write) {
            validate_path(path)?;
        }
        Ok(())
    }
}

fn validate_path(path: &Path) -> Result<(), ProcessError> {
    if !path.is_absolute() {
        return Err(ProcessError::InvalidSpec(
            "confinement paths must be absolute",
        ));
    }
    if contains_nul(path.as_os_str()) {
        return Err(ProcessError::InvalidSpec(
            "confinement paths must not contain NUL",
        ));
    }
    Ok(())
}

/// The confinement that was in force for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppliedConfinement {
    /// No filesystem or network confinement: the run came from [`super::run`].
    Unconfined,
    /// A Landlock ruleset at this ABI version, plus the seccomp socket filter.
    /// ABI 4 and later also deny TCP bind and connect through Landlock; ABI 6
    /// and later also scope signals and abstract Unix sockets to the tree.
    Landlock {
        /// The Landlock ABI version the kernel reported and the ruleset used.
        abi: u32,
        /// The Unix-domain socket mode the seccomp filter enforced.
        unix_sockets: UnixSocketAccess,
    },
}
