//! Windows AppContainer identities and persistent directory grants.
//!
//! A confined Windows run executes inside an AppContainer whose SID is derived
//! from a stable [`ContainerName`]. The container can open a file only if the
//! file's DACL grants that SID, or every application package, the access it
//! asks for. A grant is one explicit inheritable allow entry for the
//! container SID on one directory. Windows propagates it to existing
//! children when it is written and new children inherit it. The grant persists
//! until [`revoke`] removes it; callers record both in a durable journal.
//!
//! Windows starts a process in an AppContainer only when the container's
//! profile exists. [`grant`] creates it; [`delete_profile`] removes it once
//! no grant remains. The profile includes a folder under
//! `%LOCALAPPDATA%\Packages` that the container itself can write.
//!
//! Grants are verified by reading the DACL back from the same handle that was
//! changed, and that handle never follows a final reparse point. Other
//! same-user programs are not isolated: they can change the same ACLs. See
//! `docs/execution.md` and ADR-0029.
//!
//! Other platforms have no AppContainer; every operation there returns
//! [`io::ErrorKind::Unsupported`].

use std::io;
use std::path::Path;

use crate::DirectoryIdentity;

#[cfg(windows)]
pub(crate) mod windows;

/// Whether this platform can grant directories to a container identity.
pub const CONTAINER_GRANTS_SUPPORTED: bool = cfg!(windows);

/// Longest container name, in ASCII characters. This is the Windows limit for
/// an AppContainer name.
pub const MAX_CONTAINER_NAME_CHARS: usize = 64;

/// A validated AppContainer name: 1 to 64 ASCII letters, digits, `.`, `-`, or
/// `_`, starting with a letter or digit. The same name always derives the same
/// container SID, so it must be stable for one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContainerName(String);

impl ContainerName {
    /// Validate `name` without contacting the operating system.
    pub fn new(name: &str) -> io::Result<Self> {
        let valid_first = name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
        let valid_rest = name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
        if !valid_first || !valid_rest || name.len() > MAX_CONTAINER_NAME_CHARS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "container name must be 1 to 64 ASCII letters, digits, '.', '-', or '_'",
            ));
        }
        Ok(Self(name.to_owned()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The access one grant gives a container on a directory and everything
/// beneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerAccess {
    /// Read, list, traverse, and execute. Toolchain roots belong here.
    ReadExecute,
    /// Also create, write, rename, and delete. No `WRITE_DAC` or
    /// `WRITE_OWNER`, so the container cannot change ACLs it does not own.
    ReadWrite,
}

/// The container SID for `name`, in string form such as `S-1-15-2-…`.
pub fn container_sid(name: &ContainerName) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows::sid_string(&windows::derive_sid(name)?)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err(unsupported())
    }
}

/// Grant `access` on the directory at `path` to the container `name`, and
/// create the container's profile if it does not exist.
///
/// The directory is opened without following a final reparse point and must
/// match `expected`. Any explicit entry for the container SID is replaced, so
/// the directory ends with exactly one. Windows propagates the inheritable
/// entry to existing children; that can take time on a large tree. The entry
/// is read back from the same handle before success is returned. A failure
/// leaves the outcome unknown; callers keep the grant revocable.
pub fn grant(
    path: &Path,
    expected: DirectoryIdentity,
    name: &ContainerName,
    access: ContainerAccess,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::grant(path, expected, name, access)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, expected, name, access);
        Err(unsupported())
    }
}

/// Remove every explicit allow entry for the container `name` from the
/// directory at `path`, which must match `expected`, and confirm none remain.
/// Windows recomputes inherited entries beneath it. Removing nothing succeeds.
pub fn revoke(path: &Path, expected: DirectoryIdentity, name: &ContainerName) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::revoke(path, expected, name)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, expected, name);
        Err(unsupported())
    }
}

/// Whether the AppContainer profile for `name` exists.
pub fn profile_exists(name: &ContainerName) -> io::Result<bool> {
    #[cfg(windows)]
    {
        windows::profile_exists(name)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err(unsupported())
    }
}

/// Delete the AppContainer profile for `name` and its container-writable
/// folder. A missing profile succeeds. Grants on directories are not changed,
/// so callers delete the profile only after the last grant is revoked.
pub fn delete_profile(name: &ContainerName) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::delete_profile(name)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err(unsupported())
    }
}

/// The access the container `name` has on the object at `path` through
/// explicit or inherited allow entries for its SID or for all application
/// packages, less any deny entries for them. `None` means less than
/// [`ContainerAccess::ReadExecute`].
pub fn granted_access(path: &Path, name: &ContainerName) -> io::Result<Option<ContainerAccess>> {
    #[cfg(windows)]
    {
        windows::granted_access(path, &windows::derive_sid(name)?)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, name);
        Err(unsupported())
    }
}

#[cfg(not(windows))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "AppContainer grants exist only on Windows",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_names_are_bounded_ascii() {
        for valid in [
            "a",
            "OMGCode.Workspace.0123abcd",
            "x-y_z.9",
            &"a".repeat(64),
        ] {
            assert_eq!(ContainerName::new(valid).expect(valid).as_str(), valid);
        }
        for invalid in [
            "",
            ".hidden",
            "-dash",
            "has space",
            "back\\slash",
            "nul\0",
            "é",
            &"a".repeat(65),
        ] {
            assert!(ContainerName::new(invalid).is_err(), "{invalid:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_refuse_every_grant_operation() {
        let name = ContainerName::new("OMGCode.Test").expect("name");
        let identity = DirectoryIdentity {
            volume: 0,
            file_id: [0; 16],
        };
        let path = Path::new("/");
        let errors = [
            container_sid(&name).expect_err("sid"),
            grant(path, identity, &name, ContainerAccess::ReadWrite).expect_err("grant"),
            revoke(path, identity, &name).expect_err("revoke"),
            granted_access(path, &name).expect_err("check"),
            profile_exists(&name).expect_err("profile"),
            delete_profile(&name).expect_err("delete profile"),
        ];
        for error in errors {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
        const { assert!(!CONTAINER_GRANTS_SUPPORTED) };
    }
}
