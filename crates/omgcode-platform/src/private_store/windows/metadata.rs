//! Security and metadata that Windows replacement can preserve or must refuse.
//!
//! A stage carries the source's owner, group, DACL, and content-indexing opt-out.
//! Anything a stage could not carry over is refused rather than dropped. An audit SACL needs
//! `SeSecurityPrivilege` to read, so it cannot be observed and is not preserved.

use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::io::AsRawHandle;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_EA_INFORMATION, FileEaInformation, NtQueryInformationFile,
};
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    ATTRIBUTE_SECURITY_INFORMATION, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
    GetSecurityDescriptorControl, LABEL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SCOPE_SECURITY_INFORMATION,
    SE_DACL_AUTO_INHERIT_REQ, SE_DACL_AUTO_INHERITED, SetKernelObjectSecurity,
    SetSecurityDescriptorControl,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED,
    FILE_BASIC_INFO, FILE_STREAM_INFO, FileBasicInfo, FileStreamInfo, GetFileInformationByHandleEx,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{ObjectSecurity, Sid, current_user_sid, default_owner_sid, invalid_data};
use crate::broker::windows::{
    native_error, query_attributes, query_identity, validate_regular_file,
};
use crate::private_store::{NativeSecurity, PrivateFileIdentity, StageMetadata};

const PRESERVED_SECURITY: OBJECT_SECURITY_INFORMATION =
    OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
const INSPECTED_SECURITY: OBJECT_SECURITY_INFORMATION = PRESERVED_SECURITY
    | LABEL_SECURITY_INFORMATION
    | ATTRIBUTE_SECURITY_INFORMATION
    | SCOPE_SECURITY_INFORMATION;
const SUPPORTED_ATTRIBUTES: u32 =
    FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;
const MAX_DESCRIPTOR_UNITS: usize = 16 * 1024;
const STREAM_BUFFER_BYTES: usize = 4096;
const DEFAULT_STREAM: &str = "::$DATA";

/// Reads everything a stage must reproduce from this open handle.
pub(crate) fn read_native_security(file: &File) -> io::Result<NativeSecurity> {
    Ok(NativeSecurity::Windows {
        descriptor: read_security(file)?,
        not_content_indexed: query_attributes(file)?.FileAttributes
            & FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
            != 0,
    })
}

/// Applies captured security and the indexing opt-out to a stage handle.
pub(crate) fn apply_native_security(file: &File, security: &NativeSecurity) -> io::Result<()> {
    let NativeSecurity::Windows {
        descriptor,
        not_content_indexed,
    } = security
    else {
        return Err(invalid_data("replacement source has foreign metadata"));
    };
    apply_security(file, descriptor)?;
    set_content_indexing(file, *not_content_indexed)
}

/// Reads the canonical owner, group, and DACL from this open handle.
///
/// Refuses what a stage could not reproduce: a missing owner, group, or DACL,
/// any label, attribute, or scope entry, and an owner this process cannot assign.
pub(crate) fn read_security(file: &File) -> io::Result<String> {
    let security = ObjectSecurity::query(file, INSPECTED_SECURITY)?;
    if security.descriptor.is_null()
        || security.owner.is_null()
        || security.group.is_null()
        || security.dacl.is_null()
    {
        return Err(unsupported(
            "replacement requires an owner, group, and DACL",
        ));
    }
    if security.has_sacl_entries() {
        return Err(unsupported(
            "replacement cannot preserve label, attribute, or scope entries",
        ));
    }
    if !assignable_owner(&security.owner()?)? {
        return Err(unsupported(
            "replacement requires a source owned by the current user",
        ));
    }
    let text = security.to_sddl(PRESERVED_SECURITY)?;
    if text.is_empty() || !text.is_ascii() {
        return Err(unsupported("replacement cannot record this descriptor"));
    }
    Ok(text)
}

/// Applies a descriptor from `read_security` to a stage handle.
///
/// The low-level call stores the descriptor as given. The higher-level
/// `SetSecurityInfo` would recompute inheritance and change inherited entries.
#[allow(unsafe_code)]
pub(crate) fn apply_security(file: &File, descriptor: &str) -> io::Result<()> {
    let text: Vec<u16> = descriptor
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut raw: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `text` is NUL-terminated and live for the call, `raw` is writable,
    // and a null size output is permitted.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = set_descriptor(file, raw);
    // SAFETY: `raw` was allocated by the conversion above, which requires
    // `LocalFree`, and is not used afterwards. A failed free can only leak.
    let _ = unsafe { LocalFree(raw) };
    result
}

/// Stores a converted descriptor, keeping its auto-inherited state.
///
/// Windows clears `SE_DACL_AUTO_INHERITED` unless the request also sets
/// `SE_DACL_AUTO_INHERIT_REQ`. The protected state comes from the descriptor's
/// own control bits. The caller retains ownership of `raw`.
#[allow(unsafe_code)]
fn set_descriptor(file: &File, raw: PSECURITY_DESCRIPTOR) -> io::Result<()> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `raw` is a valid descriptor and both outputs are writable.
    if unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_AUTO_INHERITED != 0 {
        // SAFETY: `raw` is a valid, writable descriptor owned by the caller.
        let set = unsafe {
            SetSecurityDescriptorControl(raw, SE_DACL_AUTO_INHERIT_REQ, SE_DACL_AUTO_INHERIT_REQ)
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: `file` is live and was opened with WRITE_DAC and WRITE_OWNER.
    // `raw` is a valid self-relative descriptor for the duration of this call.
    let applied =
        unsafe { SetKernelObjectSecurity(file.as_raw_handle() as HANDLE, PRESERVED_SECURITY, raw) };
    if applied == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets or clears the indexing opt-out. Zero time fields leave timestamps alone.
#[allow(unsafe_code)]
fn set_content_indexing(file: &File, not_content_indexed: bool) -> io::Result<()> {
    let information = FILE_BASIC_INFO {
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: FILE_ATTRIBUTE_ARCHIVE
            | if not_content_indexed {
                FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
            } else {
                0
            },
    };
    let length = u32::try_from(size_of::<FILE_BASIC_INFO>())
        .map_err(|_| invalid_data("basic information is too large"))?;
    // SAFETY: `file` is live with FILE_WRITE_ATTRIBUTES, and `information` is a
    // complete structure for this class, live for the synchronous call.
    let set = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileBasicInfo,
            std::ptr::from_ref(&information).cast(),
            length,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Refuses attributes, alternate streams, and extended attributes on this handle.
pub(crate) fn validate_file_metadata(file: &File) -> io::Result<()> {
    if query_attributes(file)?.FileAttributes & !SUPPORTED_ATTRIBUTES != 0 {
        return Err(unsupported(
            "replacement cannot preserve this file's attributes",
        ));
    }
    require_single_data_stream(file)?;
    require_no_extended_attributes(file)
}

/// Reads stage metadata from one open handle. No pathname lookup occurs.
pub(in crate::private_store) fn stage_metadata(file: &File) -> io::Result<StageMetadata> {
    let identity = query_identity(file)?;
    validate_regular_file(file, identity.VolumeSerialNumber)?;
    validate_file_metadata(file)?;
    Ok(StageMetadata {
        identity: PrivateFileIdentity {
            volume: identity.VolumeSerialNumber,
            file_id: identity.FileId.Identifier,
        },
        security: read_native_security(file)?,
    })
}

/// Reports whether this token may set `owner` without a restore privilege.
fn assignable_owner(owner: &Sid) -> io::Result<bool> {
    Ok(current_user_sid()?.equals(owner) || default_owner_sid()?.equals(owner))
}

#[allow(unsafe_code)]
fn require_single_data_stream(file: &File) -> io::Result<()> {
    let mut storage = vec![0u64; STREAM_BUFFER_BYTES / size_of::<u64>()];
    let length = u32::try_from(STREAM_BUFFER_BYTES)
        .map_err(|_| invalid_data("stream buffer is too large"))?;
    // SAFETY: `file` is live and `storage` is aligned and writable for `length`.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileStreamInfo,
            storage.as_mut_ptr().cast(),
            length,
        )
    };
    if read == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == i32::try_from(ERROR_MORE_DATA).ok() {
            return Err(unsupported("replacement cannot preserve alternate streams"));
        }
        return Err(error);
    }
    // SAFETY: the buffer is fully initialized and this view does not outlive it.
    let bytes =
        unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), STREAM_BUFFER_BYTES) };
    if !is_single_default_stream(bytes)? {
        return Err(unsupported("replacement cannot preserve alternate streams"));
    }
    Ok(())
}

/// Parses a `FILE_STREAM_INFO` chain, accepting only the unnamed data stream.
fn is_single_default_stream(bytes: &[u8]) -> io::Result<bool> {
    let next = read_u32(bytes, offset_of!(FILE_STREAM_INFO, NextEntryOffset))?;
    let name_bytes = read_u32(bytes, offset_of!(FILE_STREAM_INFO, StreamNameLength))?;
    let start = offset_of!(FILE_STREAM_INFO, StreamName);
    let end = usize::try_from(name_bytes)
        .ok()
        .and_then(|length| start.checked_add(length))
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid_data("native stream name exceeds its bounds"))?;
    let name = bytes[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    Ok(next == 0 && DEFAULT_STREAM.encode_utf16().eq(name))
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    offset
        .checked_add(size_of::<u32>())
        .and_then(|end| bytes.get(offset..end))
        .and_then(|field| <[u8; 4]>::try_from(field).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| invalid_data("native stream record is truncated"))
}

#[allow(unsafe_code)]
fn require_no_extended_attributes(file: &File) -> io::Result<()> {
    let mut information = FILE_EA_INFORMATION { EaSize: 0 };
    let mut status_block = IO_STATUS_BLOCK::default();
    let length = u32::try_from(size_of::<FILE_EA_INFORMATION>())
        .map_err(|_| invalid_data("EA information is too large"))?;
    // SAFETY: `file` is a live synchronous handle. Both outputs are writable
    // and sized for this information class for the duration of the call.
    let status = unsafe {
        NtQueryInformationFile(
            file.as_raw_handle() as HANDLE,
            &mut status_block,
            std::ptr::from_mut(&mut information).cast(),
            length,
            FileEaInformation,
        )
    };
    if status != 0 {
        return Err(native_error(status));
    }
    if information.EaSize != 0 {
        return Err(unsupported(
            "replacement cannot preserve extended attributes",
        ));
    }
    Ok(())
}

impl ObjectSecurity {
    /// Reports whether the SACL read from this handle holds any entry.
    #[allow(unsafe_code)]
    fn has_sacl_entries(&self) -> bool {
        // SAFETY: a non-null `sacl` points into `descriptor`, live until drop.
        !self.sacl.is_null() && unsafe { (*self.sacl).AceCount } != 0
    }

    /// Converts the selected parts of this descriptor to bounded SDDL text.
    #[allow(unsafe_code)]
    fn to_sddl(&self, information: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        let mut length = 0u32;
        // SAFETY: `descriptor` is live until drop; both outputs are writable.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                self.descriptor,
                SDDL_REVISION_1,
                information,
                &mut text,
                &mut length,
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: success returned a buffer of `length` units holding a
        // NUL-terminated string. `length` can exceed the text, so only the part
        // before the first NUL is used. It is read here, then freed exactly once.
        unsafe {
            let units = usize::try_from(length).unwrap_or(0);
            let buffer = std::slice::from_raw_parts(text, units);
            let end = buffer.iter().position(|unit| *unit == 0);
            let value = end
                .filter(|end| *end <= MAX_DESCRIPTOR_UNITS)
                .map(|end| String::from_utf16(&buffer[..end]));
            let _ = LocalFree(text.cast());
            match value {
                Some(Ok(value)) => Ok(value),
                _ => Err(unsupported("replacement cannot record this descriptor")),
            }
        }
    }
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{create_unprotected_file, fixture};
    use super::*;

    /// A stage starts protected, so this must clear that state, not inherit it.
    #[test]
    fn an_unprotected_descriptor_replaces_a_protected_stage_exactly() {
        let fixture = fixture();
        let source = create_unprotected_file(&fixture, "source");
        let descriptor = read_security(&source).expect("source security");
        assert!(descriptor.contains("D:("), "unprotected: {descriptor}");
        let stage = fixture.root.create_stage().expect("stage");
        assert!(read_security(&stage).expect("stage").contains("D:P("));
        apply_security(&stage, &descriptor).expect("apply");
        assert_eq!(read_security(&stage).expect("applied"), descriptor);
    }

    fn record(next: u32, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let start = offset_of!(FILE_STREAM_INFO, StreamName);
        let mut bytes = vec![0u8; start + units.len() * 2];
        bytes[..4].copy_from_slice(&next.to_le_bytes());
        let length = u32::try_from(units.len() * 2).expect("name length");
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        for (index, unit) in units.iter().enumerate() {
            bytes[start + index * 2..start + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn stream_records_accept_only_one_default_stream() {
        assert!(is_single_default_stream(&record(0, "::$DATA")).expect("default"));
        assert!(!is_single_default_stream(&record(40, "::$DATA")).expect("chained"));
        assert!(!is_single_default_stream(&record(0, ":extra:$DATA")).expect("named"));
    }

    #[test]
    fn truncated_stream_records_are_invalid() {
        assert!(is_single_default_stream(&[0u8; 4]).is_err());
        let mut bytes = record(0, "::$DATA");
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(is_single_default_stream(&bytes).is_err());
    }
}
