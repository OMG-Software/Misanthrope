//! Windows private storage with verified per-user ACLs on NTFS.
//!
//! This backend is not yet reachable from the core or CLI. `PrivateDirectory`
//! still refuses on Windows, so no checkpoint or replacement is enabled here.
//!
//! Every entry this store creates carries an explicit protected DACL granting
//! only the current user, applied by the same native create. The application
//! data root inherits its ACL, so it instead must grant nothing beyond the
//! current user, SYSTEM, and Administrators. ACLs are verified from the open
//! handle and never repaired.
//!
//! Directory durability has no Microsoft contract. `sync` flushes the directory
//! handle and refuses on failure, but no page states that this persists entries
//! or renames. See ADR-0023. Same-user and administrator actors are not isolated.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::time::SystemTime;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAce, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetAclInformation, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
    INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR,
    SECURITY_MAX_SID_SIZE, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ACCESS_RIGHTS, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, GetVolumeInformationByHandleW,
    READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::PrivateFileIdentity;
use crate::broker::invalid_windows_name;
use crate::broker::windows::{
    DIRECTORY_ACCESS, SHARE_READ_WRITE, create_relative_secured, creation_time, is_dot_entry,
    is_name_not_found, open_relative, query_directory_batch, query_identity, validate_directory,
    validate_regular_file,
};
use crate::{DirectoryIdentity, directory_identity};

const PRIVATE_DIRECTORY_ACCESS: FILE_ACCESS_RIGHTS =
    DIRECTORY_ACCESS | READ_CONTROL | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY;
const FILE_READ_ACCESS: FILE_ACCESS_RIGHTS =
    FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const FILE_WRITE_ACCESS: FILE_ACCESS_RIGHTS =
    FILE_READ_ACCESS | FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_ATTRIBUTES;
const DIRECTORY_OPTIONS: u32 =
    FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
const FILE_OPTIONS: u32 =
    FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
const MAX_COMPONENT_UNITS: usize = 255;
const MAX_ROOT_ACES: u32 = 64;
const MAX_TOKEN_USER_BYTES: u32 = 1024;
const FILE_SYSTEM_NAME_UNITS: usize = 32;

/// An owned copy of a native SID in aligned storage.
#[derive(Clone)]
struct Sid {
    storage: Vec<u64>,
}

impl Sid {
    /// Copies a validated SID so its lifetime no longer depends on `raw`.
    #[allow(unsafe_code)]
    fn copy_from(raw: PSID) -> io::Result<Self> {
        // SAFETY: callers pass a SID pointer returned by a successful Windows
        // call. `IsValidSid` validates its structure before its length is used.
        let length = unsafe {
            if IsValidSid(raw) == 0 {
                return Err(invalid_data("native SID is invalid"));
            }
            GetLengthSid(raw)
        };
        if length == 0 || length > SECURITY_MAX_SID_SIZE {
            return Err(invalid_data("native SID has an invalid length"));
        }
        let bytes = usize::try_from(length).map_err(|_| invalid_data("SID length overflow"))?;
        let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
        // SAFETY: `raw` is valid for `bytes`, per `GetLengthSid`. `storage` was
        // just allocated with at least that capacity and cannot overlap it.
        unsafe {
            std::ptr::copy_nonoverlapping(raw.cast::<u8>(), storage.as_mut_ptr().cast(), bytes);
        }
        Ok(Self { storage })
    }

    /// Builds one machine-independent well-known SID.
    #[allow(unsafe_code)]
    fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Self> {
        let mut size = SECURITY_MAX_SID_SIZE;
        let capacity = usize::try_from(size).map_err(|_| invalid_data("SID size overflow"))?;
        let mut storage = vec![0u64; capacity.div_ceil(size_of::<u64>())];
        // SAFETY: `storage` is writable and aligned for `size` bytes, and `size`
        // is live for the call. A null domain is valid for these SID kinds.
        let created = unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                storage.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    /// Exposes the SID for read-only native calls while `self` is alive.
    fn as_psid(&self) -> PSID {
        self.storage.as_ptr().cast_mut().cast()
    }

    #[allow(unsafe_code)]
    fn length(&self) -> io::Result<usize> {
        // SAFETY: `self` holds a SID validated or produced by Windows.
        let length = unsafe { GetLengthSid(self.as_psid()) };
        usize::try_from(length).map_err(|_| invalid_data("SID length overflow"))
    }

    #[allow(unsafe_code)]
    fn equals(&self, other: &Sid) -> bool {
        // SAFETY: both values hold valid SIDs that outlive this call.
        unsafe { EqualSid(self.as_psid(), other.as_psid()) != 0 }
    }
}

/// The principals this store trusts, resolved once from the process token.
#[derive(Clone)]
struct Principals {
    user: Sid,
    system: Sid,
    administrators: Sid,
}

impl Principals {
    fn current() -> io::Result<Self> {
        Ok(Self {
            user: current_user_sid()?,
            system: Sid::well_known(WinLocalSystemSid)?,
            administrators: Sid::well_known(WinBuiltinAdministratorsSid)?,
        })
    }

    fn trusted(&self, sid: &Sid) -> bool {
        self.user.equals(sid) || self.system.equals(sid) || self.administrators.equals(sid)
    }
}

/// Reads the process token's user SID.
#[allow(unsafe_code)]
fn current_user_sid() -> io::Result<Sid> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle is always valid and `raw` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success returned a new token handle that this value now owns.
    let token = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    let mut needed = 0u32;
    // SAFETY: a null buffer with zero length is the documented size query.
    let sized = unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    let size_error = io::Error::last_os_error();
    if sized != 0
        || size_error.raw_os_error() != i32::try_from(ERROR_INSUFFICIENT_BUFFER).ok()
        || !(1..=MAX_TOKEN_USER_BYTES).contains(&needed)
    {
        return Err(invalid_data("process token user size is invalid"));
    }
    let bytes = usize::try_from(needed).map_err(|_| invalid_data("token size overflow"))?;
    let mut storage = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    // SAFETY: `storage` is aligned and writable for `needed` bytes.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success wrote a `TOKEN_USER` at the start of aligned `storage`,
    // and its SID pointer refers into `storage`, which is still alive here.
    Sid::copy_from(unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid })
}

/// An absolute security descriptor and the buffers it points into.
struct ExplicitDescriptor {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u64>,
    _owner: Sid,
}

impl ExplicitDescriptor {
    /// Builds a DACL granting full access to exactly `grantees`.
    ///
    /// Heap storage keeps the internal pointers stable if this value moves.
    #[allow(unsafe_code)]
    fn new(owner: &Sid, grantees: &[&Sid], protected: bool) -> io::Result<Self> {
        let mut acl_bytes = size_of::<ACL>();
        for grantee in grantees {
            acl_bytes = acl_bytes
                .checked_add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .and_then(|bytes| bytes.checked_add(grantee.length().ok()?))
                .ok_or_else(|| invalid_data("ACL size overflow"))?;
        }
        let acl_length = u32::try_from(acl_bytes).map_err(|_| invalid_data("ACL too large"))?;
        let mut acl = vec![0u64; acl_bytes.div_ceil(size_of::<u64>())];
        let acl_pointer = acl.as_mut_ptr().cast::<ACL>();
        let owner = owner.clone();
        let mut descriptor = Box::new(SECURITY_DESCRIPTOR {
            Revision: 0,
            Sbz1: 0,
            Control: 0,
            Owner: std::ptr::null_mut(),
            Group: std::ptr::null_mut(),
            Sacl: std::ptr::null_mut(),
            Dacl: std::ptr::null_mut(),
        });
        let raw: PSECURITY_DESCRIPTOR = std::ptr::from_mut(descriptor.as_mut()).cast();
        // SAFETY: `acl` is aligned and sized for the header and every ACE
        // computed above. Each SID is valid and `AddAccessAllowedAce` copies it.
        unsafe {
            if InitializeAcl(acl_pointer, acl_length, ACL_REVISION) == 0 {
                return Err(io::Error::last_os_error());
            }
            for grantee in grantees {
                if AddAccessAllowedAce(
                    acl_pointer,
                    ACL_REVISION,
                    FILE_ALL_ACCESS,
                    grantee.as_psid(),
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        // SAFETY: `raw` is a live, writable descriptor. Windows stores `owner`
        // and `acl` by reference; `Self` keeps both alive with stable heap
        // addresses for as long as the descriptor can be used.
        unsafe {
            if InitializeSecurityDescriptor(raw, SECURITY_DESCRIPTOR_REVISION) == 0
                || SetSecurityDescriptorOwner(raw, owner.as_psid(), 0) == 0
                || SetSecurityDescriptorDacl(raw, 1, acl_pointer, 0) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if protected
                && SetSecurityDescriptorControl(raw, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            descriptor,
            _acl: acl,
            _owner: owner,
        })
    }

    fn private_to(user: &Sid) -> io::Result<Self> {
        Self::new(user, &[user], true)
    }
}

/// One decoded ACE. Only the fields used by the privacy checks are kept.
enum Ace {
    Allowed { flags: u32, mask: u32, sid: Sid },
    Denied,
    Other { flags: u32 },
}

/// A security descriptor read from an open handle; freed exactly once on drop.
struct ObjectSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}

impl ObjectSecurity {
    /// Reads the owner and DACL of the object behind this same open handle.
    #[allow(unsafe_code)]
    fn read(file: &File) -> io::Result<Self> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `file` is a live handle opened with READ_CONTROL, and every
        // output pointer is writable for this synchronous call.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(status).map_err(|_| invalid_data("native error out of range"))?,
            ));
        }
        let security = Self {
            descriptor,
            owner,
            dacl,
        };
        if security.descriptor.is_null() || security.owner.is_null() || security.dacl.is_null() {
            return Err(invalid_data("object has no owner or DACL"));
        }
        Ok(security)
    }

    fn owner(&self) -> io::Result<Sid> {
        Sid::copy_from(self.owner)
    }

    #[allow(unsafe_code)]
    fn dacl_protected(&self) -> io::Result<bool> {
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: `descriptor` is live until drop; both outputs are writable.
        let read =
            unsafe { GetSecurityDescriptorControl(self.descriptor, &mut control, &mut revision) };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(control & SE_DACL_PROTECTED != 0)
    }

    #[allow(unsafe_code)]
    fn aces(&self) -> io::Result<Vec<Ace>> {
        let mut information = ACL_SIZE_INFORMATION {
            AceCount: 0,
            AclBytesInUse: 0,
            AclBytesFree: 0,
        };
        let length = u32::try_from(size_of::<ACL_SIZE_INFORMATION>())
            .map_err(|_| invalid_data("ACL information too large"))?;
        // SAFETY: `dacl` is live until drop; `information` is writable.
        let read = unsafe {
            GetAclInformation(
                self.dacl,
                std::ptr::from_mut(&mut information).cast(),
                length,
                AclSizeInformation,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        if information.AceCount > MAX_ROOT_ACES {
            return Err(invalid_data("object DACL has too many entries"));
        }
        let mut aces = Vec::new();
        for index in 0..information.AceCount {
            let mut raw = std::ptr::null_mut();
            // SAFETY: `index` is below the reported ACE count, and `raw` is
            // writable. The returned pointer refers into `dacl`.
            if unsafe { GetAce(self.dacl, index, &mut raw) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a successful `GetAce` yields a pointer to an ACE header
            // inside the live DACL. Only the header is read before its type is known.
            let header = unsafe { std::ptr::read_unaligned(raw.cast::<ACE_HEADER>()) };
            let flags = u32::from(header.AceFlags);
            let kind = u32::from(header.AceType);
            aces.push(if kind == ACCESS_ALLOWED_ACE_TYPE {
                let ace = raw.cast::<ACCESS_ALLOWED_ACE>();
                // SAFETY: the type check confirms this layout. `SidStart` is
                // the first word of a SID that `copy_from` validates.
                let (mask, sid) = unsafe {
                    (
                        std::ptr::read_unaligned(std::ptr::addr_of!((*ace).Mask)),
                        Sid::copy_from(std::ptr::addr_of!((*ace).SidStart).cast_mut().cast())?,
                    )
                };
                Ace::Allowed { flags, mask, sid }
            } else if kind == ACCESS_DENIED_ACE_TYPE {
                Ace::Denied
            } else {
                Ace::Other { flags }
            });
        }
        Ok(aces)
    }
}

impl Drop for ObjectSecurity {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `descriptor` came from `GetSecurityInfo`, which requires
        // `LocalFree`, and is freed only here. The pointers into it are not used
        // afterwards. The result is discarded because a failed free can only
        // leak; `Drop` has no way to report it and must not panic.
        let _ = unsafe { LocalFree(self.descriptor) };
    }
}

/// Requires the exact private DACL this store creates, read from `file`.
fn require_private(file: &File, principals: &Principals) -> io::Result<()> {
    let security = ObjectSecurity::read(file)?;
    let exact = security.dacl_protected()?
        && principals.user.equals(&security.owner()?)
        && matches!(
            security.aces()?.as_slice(),
            [Ace::Allowed { flags: 0, mask, sid }]
                if *mask == FILE_ALL_ACCESS && principals.user.equals(sid)
        );
    if !exact {
        return Err(invalid_data(
            "private object owner or DACL is not exactly the current user",
        ));
    }
    Ok(())
}

/// Requires an owner and effective grants limited to trusted principals.
///
/// Inherit-only entries do not grant access on this object, and this store's
/// children carry protected DACLs that cannot inherit them.
fn require_trusted_root(file: &File, principals: &Principals) -> io::Result<()> {
    let security = ObjectSecurity::read(file)?;
    if !principals.trusted(&security.owner()?) {
        return Err(invalid_data("private root owner is not trusted"));
    }
    for ace in security.aces()? {
        let acceptable = match ace {
            Ace::Allowed { flags, .. } if flags & INHERIT_ONLY_ACE != 0 => true,
            Ace::Allowed { sid, .. } => principals.trusted(&sid),
            Ace::Denied => true,
            Ace::Other { flags } => flags & INHERIT_ONLY_ACE != 0,
        };
        if !acceptable {
            return Err(invalid_data("private root grants an untrusted principal"));
        }
    }
    Ok(())
}

/// Refuses anything except NTFS, the only filesystem this backend has tested.
#[allow(unsafe_code)]
fn require_ntfs(file: &File) -> io::Result<()> {
    let mut name = [0u16; FILE_SYSTEM_NAME_UNITS];
    let capacity = u32::try_from(name.len()).map_err(|_| invalid_data("name buffer too large"))?;
    // SAFETY: `file` is live and `name` is writable for `capacity` units. The
    // optional outputs are null, which the API permits.
    let read = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            capacity,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    if !"NTFS".encode_utf16().eq(name.iter().copied().take(length)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private storage requires NTFS on Windows",
        ));
    }
    Ok(())
}

/// Which ACL rule applies to a retained directory.
#[derive(Clone, Copy)]
enum Scope {
    Root,
    Private,
}

/// A retained private directory on NTFS. Owned handles close on drop.
///
/// Handles deny delete sharing, which blocks ordinary rename and delete while
/// held. The caller must compare the root identity with its trusted
/// registration and own any required writer lock.
pub struct WindowsPrivateDirectory {
    file: File,
    volume: u64,
    scope: Scope,
    principals: Principals,
}

impl WindowsPrivateDirectory {
    /// Opens an existing absolute root without following a final reparse point.
    ///
    /// Earlier path components are trusted here; callers bind the opened object
    /// to its registered identity. Unsupported filesystems and untrusted ACLs
    /// are refused, and nothing is created or repaired.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = open_root_handle(path)?;
        let volume = query_identity(&file)?.VolumeSerialNumber;
        require_ntfs(&file)?;
        let root = Self {
            file,
            volume,
            scope: Scope::Root,
            principals: Principals::current()?,
        };
        root.validate()?;
        Ok(root)
    }

    /// Returns the opened directory identity after rechecking its ACL.
    pub fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.validate()?;
        directory_identity(&self.file)
    }

    /// Returns native creation time after rechecking its ACL.
    pub fn created(&self) -> io::Result<SystemTime> {
        self.validate()?;
        creation_time(&self.file)
    }

    /// Opens one existing private child directory relative to this handle.
    pub fn open_dir(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        self.validate()?;
        let file = open_relative(
            &self.file,
            name,
            PRIVATE_DIRECTORY_ACCESS,
            FILE_OPEN,
            DIRECTORY_OPTIONS,
            SHARE_READ_WRITE,
            FILE_ATTRIBUTE_DIRECTORY,
        )
        .map_err(map_missing)?;
        self.child(file)
    }

    /// Exclusively creates one private directory and flushes its parent.
    /// Any error may leave an entry. Nothing is retried or deleted.
    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        validate_name(name)?;
        self.validate()?;
        let security = ExplicitDescriptor::private_to(&self.principals.user)?;
        let file = create_relative_secured(
            &self.file,
            name,
            PRIVATE_DIRECTORY_ACCESS,
            DIRECTORY_OPTIONS,
            SHARE_READ_WRITE,
            FILE_ATTRIBUTE_DIRECTORY,
            &security.descriptor,
        )?;
        let directory = self.child(file)?;
        self.sync()?;
        Ok(directory)
    }

    /// Opens a regular single-link private file through this retained handle.
    /// The caller owns the file and must bound reads and writes.
    pub fn open_file(&self, name: &OsStr, writable: bool) -> io::Result<File> {
        validate_name(name)?;
        self.validate()?;
        let access = if writable {
            FILE_WRITE_ACCESS
        } else {
            FILE_READ_ACCESS
        };
        let file = open_relative(
            &self.file,
            name,
            access,
            FILE_OPEN,
            FILE_OPTIONS,
            FILE_SHARE_READ,
            0,
        )
        .map_err(map_missing)?;
        self.validate_child_file(&file)?;
        Ok(file)
    }

    /// Exclusively creates a private file and flushes its parent.
    /// The caller must write and sync its contents. Any error may leave an entry.
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        validate_name(name)?;
        self.validate()?;
        let security = ExplicitDescriptor::private_to(&self.principals.user)?;
        let file = create_relative_secured(
            &self.file,
            name,
            FILE_WRITE_ACCESS,
            FILE_OPTIONS,
            FILE_SHARE_READ,
            FILE_ATTRIBUTE_NORMAL,
            &security.descriptor,
        )?;
        self.validate_child_file(&file)?;
        self.sync()?;
        Ok(file)
    }

    /// Observes whether one name exists without following a reparse point.
    /// This observation does not validate or authorize a later open.
    pub fn exists(&self, name: &OsStr) -> io::Result<bool> {
        validate_name(name)?;
        self.validate()?;
        match open_relative(
            &self.file,
            name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            SHARE_READ_WRITE | FILE_SHARE_DELETE,
            0,
        ) {
            Ok(_) => Ok(true),
            Err(error) if is_name_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Enumerates at most `limit` (1..=128) native names. Overflow is an error,
    /// never a truncated inventory. No child object is followed.
    pub fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        if !(1..=128).contains(&limit) {
            return Err(invalid_input("invalid private inventory bound"));
        }
        self.validate()?;
        let mut names = Vec::new();
        let mut restart = true;
        loop {
            let batch = query_directory_batch(&self.file, restart)?;
            restart = false;
            if batch.is_empty() {
                return Ok(names);
            }
            for (name, _attributes) in batch {
                if is_dot_entry(&name) {
                    continue;
                }
                if names.len() == limit {
                    return Err(invalid_data("private inventory exceeds bound"));
                }
                names.push(name);
            }
        }
    }

    /// Flushes this directory handle. A failed flush is an error, never a no-op.
    ///
    /// Microsoft does not state that this persists directory entries or renames,
    /// so success carries no power-loss guarantee. See ADR-0023.
    pub fn sync(&self) -> io::Result<()> {
        self.validate()?;
        self.file.sync_all()
    }

    fn child(&self, file: File) -> io::Result<Self> {
        let directory = Self {
            file,
            volume: self.volume,
            scope: Scope::Private,
            principals: self.principals.clone(),
        };
        directory.validate()?;
        Ok(directory)
    }

    fn validate_child_file(&self, file: &File) -> io::Result<()> {
        validate_regular_file(file, self.volume)?;
        require_private(file, &self.principals)
    }

    /// Rechecks this retained directory's type, volume, and ACL.
    fn validate(&self) -> io::Result<()> {
        validate_directory(&self.file, self.volume)?;
        match self.scope {
            Scope::Root => require_trusted_root(&self.file, &self.principals),
            Scope::Private => require_private(&self.file, &self.principals),
        }
    }
}

/// Checks privacy on the opened file before returning its native identity.
/// The handle must have READ_CONTROL. No pathname lookup or reopen occurs.
pub fn file_identity(file: &File) -> io::Result<PrivateFileIdentity> {
    let identity = query_identity(file)?;
    validate_regular_file(file, identity.VolumeSerialNumber)?;
    require_private(file, &Principals::current()?)?;
    Ok(PrivateFileIdentity {
        volume: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
    })
}

fn open_root_handle(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(invalid_input("private root must be absolute"));
    }
    OpenOptions::new()
        .access_mode(PRIVATE_DIRECTORY_ACCESS)
        .share_mode(SHARE_READ_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Accepts exactly one bounded native name, refusing Win32 aliases and streams.
fn validate_name(name: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = name.encode_wide().collect();
    if units.len() > MAX_COMPONENT_UNITS
        || units.iter().any(|unit| matches!(*unit, 47 | 92))
        || invalid_windows_name(&units)
    {
        return Err(invalid_input(
            "private operation requires one native entry name",
        ));
    }
    Ok(())
}

/// Reports an absent single name with the same kind Linux returns.
fn map_missing(error: io::Error) -> io::Error {
    if is_name_not_found(&error) {
        io::Error::new(io::ErrorKind::NotFound, "private entry does not exist")
    } else {
        error
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
    use crate::broker::windows::tests::create_junction_bounded;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::PathBuf;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::WinWorldSid;

    /// Declares `root` before `temporary` so handles close before cleanup.
    struct Fixture {
        root: WindowsPrivateDirectory,
        path: PathBuf,
        _temporary: tempfile::TempDir,
    }

    fn user() -> Sid {
        current_user_sid().expect("current user SID")
    }

    fn world() -> Sid {
        Sid::well_known(WinWorldSid).expect("world SID")
    }

    /// Creates a directory beneath `parent` with an explicit test descriptor.
    fn create_directory_with(parent: &Path, name: &str, security: &ExplicitDescriptor) -> PathBuf {
        let handle = open_root_handle(parent).expect("fixture parent");
        create_relative_secured(
            &handle,
            OsStr::new(name),
            PRIVATE_DIRECTORY_ACCESS,
            DIRECTORY_OPTIONS,
            SHARE_READ_WRITE,
            FILE_ATTRIBUTE_DIRECTORY,
            &security.descriptor,
        )
        .expect("fixture directory");
        parent.join(name)
    }

    fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().expect("fixture");
        let user = user();
        let security = ExplicitDescriptor::private_to(&user).expect("descriptor");
        let path = create_directory_with(temporary.path(), "root", &security);
        let root = WindowsPrivateDirectory::open(&path).expect("private root");
        Fixture {
            root,
            path,
            _temporary: temporary,
        }
    }

    /// Decodes a handle's owner and DACL independently of the checks under test.
    #[allow(unsafe_code)]
    fn sddl(file: &File) -> String {
        let security = ObjectSecurity::read(file).expect("object security");
        let mut text = std::ptr::null_mut();
        // SAFETY: `descriptor` is live and `text` is writable.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                security.descriptor,
                SDDL_REVISION_1,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0, "descriptor converts to SDDL");
        // SAFETY: success returned a NUL-terminated wide string that this test
        // reads before freeing it once with `LocalFree`, as documented.
        unsafe {
            let length = (0..).take_while(|index| *text.add(*index) != 0).count();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            let _ = LocalFree(text.cast());
            value
        }
    }

    fn assert_exact_private_sddl(text: &str) {
        let owner = text
            .strip_prefix("O:")
            .and_then(|rest| rest.split("D:").next())
            .expect("SDDL owner");
        assert_eq!(
            text.split("D:").nth(1).expect("SDDL DACL"),
            format!("P(A;;FA;;;{owner})"),
            "DACL must be protected with one full-control owner grant: {text}"
        );
    }

    #[test]
    fn private_entries_round_trip_with_exclusive_creation_and_real_flush() {
        let fixture = fixture();
        let directory = fixture
            .root
            .create_dir(OsStr::new("store"))
            .expect("private child directory");
        let mut file = directory
            .create_file(OsStr::new("ledger"))
            .expect("private file");
        file.write_all(b"record").expect("write");
        file.sync_all().expect("file flush");
        directory.sync().expect("directory flush");
        drop(file);

        assert!(directory.exists(OsStr::new("ledger")).expect("exists"));
        assert!(!directory.exists(OsStr::new("absent")).expect("absent"));
        assert_eq!(
            directory.names(4).expect("names"),
            vec![OsString::from("ledger")]
        );
        let mut reopened = directory
            .open_file(OsStr::new("ledger"), true)
            .expect("reopen");
        let mut bytes = Vec::new();
        reopened.read_to_end(&mut bytes).expect("read");
        assert_eq!(bytes, b"record");
        reopened.seek(SeekFrom::End(0)).expect("seek");
        reopened.write_all(b"-more").expect("append");
        reopened.try_lock().expect("exclusive lock on private file");

        let again = fixture.root.open_dir(OsStr::new("store")).expect("reopen");
        assert_eq!(
            again.identity().expect("identity"),
            directory.identity().expect("identity")
        );
        again.created().expect("native creation time");
    }

    #[test]
    fn created_entries_carry_an_exact_protected_single_user_dacl() {
        let fixture = fixture();
        let directory = fixture.root.create_dir(OsStr::new("store")).expect("dir");
        let file = directory.create_file(OsStr::new("file")).expect("file");
        assert_exact_private_sddl(&sddl(&directory.file));
        assert_exact_private_sddl(&sddl(&file));
    }

    #[test]
    fn exclusive_creation_never_replaces_an_existing_entry() {
        let fixture = fixture();
        let mut file = fixture.root.create_file(OsStr::new("file")).expect("file");
        file.write_all(b"kept").expect("write");
        drop(file);
        let error = fixture
            .root
            .create_file(OsStr::new("file"))
            .expect_err("collision");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(fixture.path.join("file")).expect("read"), b"kept");
    }

    #[test]
    fn broadened_or_inheritable_child_acls_are_refused() {
        let fixture = fixture();
        let user = user();
        let world = world();
        let broad = ExplicitDescriptor::new(&user, &[&user, &world], true).expect("broad");
        let unprotected = ExplicitDescriptor::new(&user, &[&user], false).expect("unprotected");
        for (name, security) in [("broad", &broad), ("unprotected", &unprotected)] {
            let file = create_relative_secured(
                &fixture.root.file,
                OsStr::new(name),
                FILE_WRITE_ACCESS,
                FILE_OPTIONS,
                FILE_SHARE_READ,
                FILE_ATTRIBUTE_NORMAL,
                &security.descriptor,
            )
            .expect("fixture file");
            assert_eq!(
                file_identity(&file).expect_err("identity refused").kind(),
                io::ErrorKind::InvalidData
            );
            drop(file);
            assert_eq!(
                fixture
                    .root
                    .open_file(OsStr::new(name), false)
                    .expect_err("open refused")
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
        let directory = create_directory_with(&fixture.path, "broad-directory", &broad);
        assert!(directory.exists());
        assert_eq!(
            fixture
                .root
                .open_dir(OsStr::new("broad-directory"))
                .err()
                .expect("directory refused")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn roots_granting_untrusted_principals_are_refused() {
        let temporary = tempfile::tempdir().expect("fixture");
        let user = user();
        let world = world();
        let broad = ExplicitDescriptor::new(&user, &[&user, &world], true).expect("broad");
        let path = create_directory_with(temporary.path(), "broad", &broad);
        assert_eq!(
            WindowsPrivateDirectory::open(&path)
                .err()
                .expect("untrusted root")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn roots_may_also_grant_system_and_administrators() {
        let temporary = tempfile::tempdir().expect("fixture");
        let principals = Principals::current().expect("principals");
        let security = ExplicitDescriptor::new(
            &principals.user,
            &[
                &principals.user,
                &principals.system,
                &principals.administrators,
            ],
            true,
        )
        .expect("descriptor");
        let path = create_directory_with(temporary.path(), "root", &security);
        WindowsPrivateDirectory::open(&path).expect("trusted root");
    }

    #[test]
    fn relative_roots_are_refused_before_io() {
        assert_eq!(
            WindowsPrivateDirectory::open(Path::new("relative"))
                .err()
                .expect("relative root")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn junction_roots_and_children_are_refused() {
        let fixture = fixture();
        let outside = tempfile::tempdir().expect("outside");
        let child = fixture.path.join("junction");
        create_junction_bounded(&child, outside.path()).expect("child junction");
        assert!(
            fixture.root.open_dir(OsStr::new("junction")).is_err(),
            "a junction must not open as a private directory"
        );
        assert!(
            fixture
                .root
                .open_file(OsStr::new("junction"), false)
                .is_err(),
            "a junction must not open as a private file"
        );

        let link = outside.path().join("root-junction");
        create_junction_bounded(&link, &fixture.path).expect("root junction");
        assert!(
            WindowsPrivateDirectory::open(&link).is_err(),
            "a junction must not open as a private root"
        );
    }

    #[test]
    fn hard_linked_private_files_never_supply_data() {
        let fixture = fixture();
        let file = fixture.root.create_file(OsStr::new("file")).expect("file");
        drop(file);
        fs::hard_link(fixture.path.join("file"), fixture.path.join("alias")).expect("hard link");
        assert_eq!(
            fixture
                .root
                .open_file(OsStr::new("file"), false)
                .expect_err("linked file refused")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn missing_entries_report_not_found_without_creating_anything() {
        let fixture = fixture();
        let error = fixture
            .root
            .open_file(OsStr::new("absent"), false)
            .expect_err("missing file");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = fixture
            .root
            .open_dir(OsStr::new("absent"))
            .err()
            .expect("missing directory");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(fixture.root.names(4).expect("names").is_empty());
    }

    #[test]
    fn invalid_names_and_inventory_overflow_fail_instead_of_truncating() {
        let fixture = fixture();
        for name in ["", ".", "..", "a/b", "a\\b", "a:stream", "NUL", "trailing."] {
            assert_eq!(
                fixture
                    .root
                    .create_file(OsStr::new(name))
                    .expect_err("invalid name")
                    .kind(),
                io::ErrorKind::InvalidInput,
                "{name:?} must be refused"
            );
        }
        for name in ["one", "two"] {
            drop(fixture.root.create_file(OsStr::new(name)).expect("file"));
        }
        assert_eq!(
            fixture.root.names(1).expect_err("overflow").kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fixture.root.names(0).expect_err("bound").kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fixture.root.names(2).expect("names").len(), 2);
    }

    #[test]
    fn retained_handles_block_rename_of_a_private_directory() {
        let fixture = fixture();
        let directory = fixture.root.create_dir(OsStr::new("store")).expect("dir");
        assert!(
            fs::rename(fixture.path.join("store"), fixture.path.join("moved")).is_err(),
            "a retained private directory must not be renamed"
        );
        directory
            .validate()
            .expect("retained directory stays valid");
    }

    #[test]
    fn file_identity_uses_the_same_handle_and_distinguishes_files() {
        let fixture = fixture();
        let first = fixture
            .root
            .create_file(OsStr::new("first"))
            .expect("first");
        let second = fixture
            .root
            .create_file(OsStr::new("second"))
            .expect("second");
        let first_identity = file_identity(&first).expect("identity");
        assert_eq!(first_identity, file_identity(&first).expect("repeatable"));
        assert_ne!(first_identity, file_identity(&second).expect("identity"));
    }
}
