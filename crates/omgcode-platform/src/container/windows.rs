//! AppContainer SIDs and DACL grants on NTFS.
//!
//! The container SID is derived from its name with
//! `DeriveAppContainerSidFromAppContainerName`. Windows starts a process in
//! an AppContainer only when its profile exists, so the first grant creates
//! the profile with `CreateAppContainerProfile`. The profile adds a per-user
//! registry mapping and a container-writable folder under
//! `%LOCALAPPDATA%\Packages`; `DeleteAppContainerProfile` removes both.
//! Grants use `SetEntriesInAclW` and `SetSecurityInfo` on a handle
//! opened without following a final reparse point. `SetSecurityInfo` keeps
//! the DACL's protected or unprotected state and propagates inheritable
//! entries to existing children.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT,
    SET_ACCESS, SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
    DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
};
use windows_sys::Win32::Security::{
    ACE_FLAGS, ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, FreeSid, INHERIT_ONLY_ACE,
    INHERITED_ACE, NO_INHERITANCE, OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    UNPROTECTED_DACL_SECURITY_INFORMATION, WinBuiltinAnyPackageSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::core::HRESULT;

use super::{ContainerAccess, ContainerName};
use crate::broker::windows::{SHARE_READ_WRITE, validate_directory};
use crate::private_store::windows::{Ace, ObjectSecurity, Sid, invalid_data, require_ntfs};
use crate::{DirectoryIdentity, directory_identity};

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`: read, list, traverse, execute.
pub(crate) const READ_EXECUTE_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
/// Modify without `WRITE_DAC`, `WRITE_OWNER`, or `FILE_DELETE_CHILD`.
pub(crate) const READ_WRITE_MASK: u32 = READ_EXECUTE_MASK | FILE_GENERIC_WRITE | DELETE;
/// Directories and files beneath the granted directory inherit the entry.
const GRANT_INHERITANCE: ACE_FLAGS = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
/// Enough to verify a directory and replace its DACL; no data access.
const GRANT_ACCESS: FILE_ACCESS_RIGHTS =
    READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Enough to read an object's DACL and attributes.
const CHECK_ACCESS: FILE_ACCESS_RIGHTS = READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Bound on decoded entries in a workspace or toolchain DACL.
const MAX_GRANT_ACES: u32 = 1024;
/// Bound on a string SID; real container SIDs are under 100 units.
const MAX_SID_STRING_UNITS: usize = 256;
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`.
const PROFILE_EXISTS: HRESULT = 0x8007_00B7_u32 as HRESULT;
/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
const PROFILE_MISSING: HRESULT = 0x8007_0002_u32 as HRESULT;
const PROFILE_DESCRIPTION: &str = "OMGCode workspace container";

fn wide_nul(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn hresult_error(context: &str, result: HRESULT) -> io::Error {
    io::Error::other(format!("{context} failed: {result:#010x}"))
}

/// Derive the container SID for `name`. This needs no profile.
#[allow(unsafe_code)]
pub(crate) fn derive_sid(name: &ContainerName) -> io::Result<Sid> {
    let wide = wide_nul(name.as_str());
    let mut raw: PSID = null_mut();
    // SAFETY: `wide` is NUL-terminated UTF-16 that outlives the call, and
    // `raw` is writable.
    let result = unsafe { DeriveAppContainerSidFromAppContainerName(wide.as_ptr(), &raw mut raw) };
    if result < 0 || raw.is_null() {
        return Err(hresult_error("container SID derivation", result));
    }
    let copied = Sid::copy_from(raw);
    // SAFETY: `raw` came from the derivation call, which requires `FreeSid`,
    // and is not used after this. A failed free can only leak.
    let _ = unsafe { FreeSid(raw) };
    copied
}

/// Create the AppContainer profile for `name` with no capabilities, or keep
/// the existing one. Returns whether a profile was created.
#[allow(unsafe_code)]
pub(crate) fn ensure_profile(name: &ContainerName) -> io::Result<bool> {
    let wide = wide_nul(name.as_str());
    let description = wide_nul(PROFILE_DESCRIPTION);
    let mut raw: PSID = null_mut();
    // SAFETY: both strings are NUL-terminated and outlive the call; a null
    // capability list with a zero count requests no capabilities; `raw` is
    // writable.
    let result = unsafe {
        CreateAppContainerProfile(
            wide.as_ptr(),
            wide.as_ptr(),
            description.as_ptr(),
            null(),
            0,
            &raw mut raw,
        )
    };
    if result == PROFILE_EXISTS {
        return Ok(false);
    }
    if result < 0 || raw.is_null() {
        return Err(hresult_error("container profile creation", result));
    }
    // SAFETY: `raw` came from `CreateAppContainerProfile`, which requires
    // `FreeSid`, and is not used after this. A failed free can only leak.
    let _ = unsafe { FreeSid(raw) };
    Ok(true)
}

/// Whether the AppContainer profile for `name` exists.
#[allow(unsafe_code)]
pub(crate) fn profile_exists(name: &ContainerName) -> io::Result<bool> {
    let sid = wide_nul(&sid_string(&derive_sid(name)?)?);
    let mut path: *mut u16 = null_mut();
    // SAFETY: `sid` is a NUL-terminated string SID that outlives the call,
    // and `path` is writable.
    let result = unsafe { GetAppContainerFolderPath(sid.as_ptr(), &raw mut path) };
    if !path.is_null() {
        // SAFETY: the path was allocated by the call with the COM task
        // allocator and is not used after this.
        unsafe { CoTaskMemFree(path.cast()) };
    }
    match result {
        0 => Ok(true),
        PROFILE_MISSING => Ok(false),
        _ => Err(hresult_error("container profile lookup", result)),
    }
}

/// Delete the AppContainer profile for `name`, including its folder. A
/// missing profile succeeds. Directory grants are not changed.
#[allow(unsafe_code)]
pub(crate) fn delete_profile(name: &ContainerName) -> io::Result<()> {
    let wide = wide_nul(name.as_str());
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let result = unsafe { DeleteAppContainerProfile(wide.as_ptr()) };
    if result < 0 && result != PROFILE_MISSING {
        return Err(hresult_error("container profile deletion", result));
    }
    if profile_exists(name)? {
        return Err(invalid_data(
            "the container profile remained after deletion",
        ));
    }
    Ok(())
}

/// The SID for every application package (`S-1-15-2-1`).
pub(crate) fn all_packages_sid() -> io::Result<Sid> {
    Sid::well_known(WinBuiltinAnyPackageSid)
}

/// Format `sid` as `S-1-…`.
#[allow(unsafe_code)]
pub(crate) fn sid_string(sid: &Sid) -> io::Result<String> {
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is a valid SID for the call and `text` is writable.
    if unsafe { ConvertSidToStringSidW(sid.as_psid(), &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut units = Vec::new();
    let mut terminated = false;
    for index in 0..MAX_SID_STRING_UNITS {
        // SAFETY: success returned a NUL-terminated string. Reading stops at
        // its terminator, and the bound stops a malformed result.
        let unit = unsafe { *text.add(index) };
        if unit == 0 {
            terminated = true;
            break;
        }
        units.push(unit);
    }
    // SAFETY: the string came from `ConvertSidToStringSidW`, which requires
    // `LocalFree`, and is not used after this.
    let _ = unsafe { LocalFree(text.cast()) };
    if !terminated {
        return Err(invalid_data("SID string is too long"));
    }
    String::from_utf16(&units).map_err(|_| invalid_data("SID string is not UTF-16"))
}

pub(crate) fn grant(
    path: &Path,
    expected: DirectoryIdentity,
    name: &ContainerName,
    access: ContainerAccess,
) -> io::Result<()> {
    let sid = derive_sid(name)?;
    let directory = open_granted_directory(path, expected)?;
    ensure_profile(name)?;
    let mask = mask(access);
    set_entry(&directory, &sid, SET_ACCESS, mask)?;
    let entries = explicit_entries(&directory, &sid)?;
    if entries
        != [Explicit::Allowed {
            flags: GRANT_INHERITANCE,
            mask,
        }]
    {
        return Err(invalid_data(
            "the container grant did not read back as one exact entry",
        ));
    }
    Ok(())
}

pub(crate) fn revoke(
    path: &Path,
    expected: DirectoryIdentity,
    name: &ContainerName,
) -> io::Result<()> {
    let sid = derive_sid(name)?;
    let directory = open_granted_directory(path, expected)?;
    set_entry(&directory, &sid, REVOKE_ACCESS, 0)?;
    if explicit_entries(&directory, &sid)?
        .iter()
        .any(|entry| matches!(entry, Explicit::Allowed { .. }))
    {
        return Err(invalid_data(
            "an explicit container entry remained after revocation",
        ));
    }
    Ok(())
}

/// Effective container access from the DACL of the object at `path`.
pub(crate) fn granted_access(path: &Path, sid: &Sid) -> io::Result<Option<ContainerAccess>> {
    let object = open_object(path)?;
    let mask = allowed_mask(&object, &[sid, &all_packages_sid()?])?;
    Ok(if mask & READ_WRITE_MASK == READ_WRITE_MASK {
        Some(ContainerAccess::ReadWrite)
    } else if mask & READ_EXECUTE_MASK == READ_EXECUTE_MASK {
        Some(ContainerAccess::ReadExecute)
    } else {
        None
    })
}

const fn mask(access: ContainerAccess) -> u32 {
    match access {
        ContainerAccess::ReadExecute => READ_EXECUTE_MASK,
        ContainerAccess::ReadWrite => READ_WRITE_MASK,
    }
}

/// Open a directory for a DACL change and confirm it is the expected one.
fn open_granted_directory(path: &Path, expected: DirectoryIdentity) -> io::Result<File> {
    require_absolute(path)?;
    let directory = OpenOptions::new()
        .access_mode(GRANT_ACCESS)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    validate_directory(&directory, expected.volume)?;
    require_ntfs(&directory)?;
    if directory_identity(&directory)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the directory is not the recorded directory",
        ));
    }
    Ok(directory)
}

/// Open any object for a DACL read, refusing a final reparse point.
fn open_object(path: &Path) -> io::Result<File> {
    require_absolute(path)?;
    let object = OpenOptions::new()
        .access_mode(CHECK_ACCESS)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let attributes = std::os::windows::fs::MetadataExt::file_attributes(&object.metadata()?);
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "confinement paths must not be reparse points",
        ));
    }
    Ok(object)
}

fn require_absolute(path: &Path) -> io::Result<()> {
    crate::reject_windows_device_path(path)?;
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "container grant paths must be absolute",
        ));
    }
    Ok(())
}

/// One explicit (not inherited) entry for a SID.
#[derive(Debug, PartialEq, Eq)]
enum Explicit {
    Allowed { flags: u32, mask: u32 },
    Denied { flags: u32, mask: u32 },
}

fn explicit_entries(object: &File, sid: &Sid) -> io::Result<Vec<Explicit>> {
    let security = ObjectSecurity::read(object)?;
    let mut entries = Vec::new();
    for ace in security.aces_within(MAX_GRANT_ACES)? {
        match ace {
            Ace::Allowed {
                flags,
                mask,
                sid: entry,
            } if flags & INHERITED_ACE == 0 && entry.equals(sid) => {
                entries.push(Explicit::Allowed { flags, mask });
            }
            Ace::Denied {
                flags,
                mask,
                sid: entry,
            } if flags & INHERITED_ACE == 0 && entry.equals(sid) => {
                entries.push(Explicit::Denied { flags, mask });
            }
            _ => {}
        }
    }
    Ok(entries)
}

/// Rights allowed to any of `sids` on this object, less rights denied to them.
/// Inherit-only entries do not apply to the object itself.
fn allowed_mask(object: &File, sids: &[&Sid]) -> io::Result<u32> {
    let security = ObjectSecurity::read(object)?;
    let mut allowed = 0u32;
    let mut denied = 0u32;
    for ace in security.aces_within(MAX_GRANT_ACES)? {
        match ace {
            Ace::Allowed { flags, mask, sid }
                if flags & INHERIT_ONLY_ACE == 0 && sids.iter().any(|item| item.equals(&sid)) =>
            {
                allowed |= mask;
            }
            Ace::Denied { flags, mask, sid }
                if flags & INHERIT_ONLY_ACE == 0 && sids.iter().any(|item| item.equals(&sid)) =>
            {
                denied |= mask;
            }
            _ => {}
        }
    }
    Ok(allowed & !denied)
}

/// A DACL allocated by `SetEntriesInAclW`; freed once on drop.
struct LocalAcl(*mut ACL);

impl Drop for LocalAcl {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from `SetEntriesInAclW`, which requires
            // `LocalFree`, and is freed only here. A failed free can only leak.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// Apply one `SET_ACCESS` or `REVOKE_ACCESS` entry for `sid` and write the
/// DACL back through the same handle, keeping its protection state.
#[allow(unsafe_code)]
fn set_entry(object: &File, sid: &Sid, mode: i32, mask: u32) -> io::Result<()> {
    let security = ObjectSecurity::read(object)?;
    let protected = security.dacl_protected()?;
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: if mode == REVOKE_ACCESS {
            NO_INHERITANCE
        } else {
            GRANT_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_psid().cast(),
        },
    };
    let mut merged: *mut ACL = null_mut();
    // SAFETY: `entry` names a valid SID that outlives the call; the old DACL
    // is live until `security` drops; `merged` is writable.
    let status = unsafe { SetEntriesInAclW(1, &raw const entry, security.dacl(), &raw mut merged) };
    let merged = LocalAcl(merged);
    if status != 0 {
        return Err(os_error(status));
    }
    if merged.0.is_null() {
        return Err(invalid_data("the merged DACL is missing"));
    }
    let information = DACL_SECURITY_INFORMATION
        | if protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
    // SAFETY: `object` is live and was opened with `WRITE_DAC`; the merged
    // DACL is valid until `merged` drops after the call. Owner, group, and
    // SACL are not changed.
    let status = unsafe {
        SetSecurityInfo(
            object.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            information,
            null_mut(),
            null_mut(),
            merged.0,
            null(),
        )
    };
    if status != 0 {
        return Err(os_error(status));
    }
    Ok(())
}

fn os_error(status: u32) -> io::Error {
    i32::try_from(status).map_or_else(
        |_| invalid_data("native error out of range"),
        io::Error::from_raw_os_error,
    )
}

#[cfg(test)]
pub(crate) mod tests;
