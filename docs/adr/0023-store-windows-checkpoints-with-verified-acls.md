# 0023: Store Windows checkpoints with verified ACLs

- Status: Accepted
- Date: 2026-11-11

## Context

ADR-0018 requires private, durable checkpoints before replacement. ADR-0021 and
ADR-0022 refused Windows because inherited ACLs are not private and a
directory-sync no-op is not persistence. Windows apply needs a private store
first, as Linux did. Its directory persistence has no equivalent to the Linux
`fsync` contract.

Microsoft's pages for `FlushFileBuffers` and `NtFlushBuffersFileEx` do not state
that a directory handle can be flushed, or that a flush persists a directory
entry or rename. The documented volume-handle flush requires administrative
privileges.

## Decision

Add a Windows private-store backend on NTFS as a separate public type, not yet
used by the core. `PrivateDirectory` still refuses on Windows, so this enables
no checkpoint, replacement, or CLI behaviour.

Every entry the store creates carries an explicit protected DACL granting
`FILE_ALL_ACCESS` only to the process token's user, who is also the owner. The
descriptor is supplied to the same `NtCreateFile` call as exclusive creation, so
the entry never has an inherited ACL. Retained handles recheck the exact owner
and DACL with `GetSecurityInfo` on each operation. Any other ACL is refused and
never repaired.

The application data root inherits its ACL, so it cannot meet the exact rule. Its
owner and effective grants must be limited to the current user, SYSTEM, and
Administrators. Those principals can already bypass this store. Other grants are
refused. Inherit-only entries are ignored because the store's protected children
cannot inherit them.

Accept a directory-handle flush as the Windows persistence step, by explicit
maintainer decision. `sync` flushes the handle, which needs `FILE_ADD_FILE`
access, and a failure is an error, never a no-op. **Microsoft gives no contract
that this persists entries or renames.** Windows therefore makes no power-loss
claim and stays unqualified for release. Refuse every filesystem except NTFS.

Reuse the existing handle-relative native opens. Enable three more `windows-sys`
features for security and token queries. The dependency version and lockfile do
not change. Translate a missing name to `NotFound` only in this store, because
the core reports that kind differently from workspace errors.

## Alternatives considered

- Require the administrator-only volume flush: rejected. It has a vendor contract
  but would make apply work only elevated and would flush the whole volume.
- Keep Windows refused until a contract exists: rejected by maintainer decision,
  since no such contract is expected and Windows is a first-release target.
- Trust inherited ACLs: rejected by ADR-0021. The exact rule applies to everything
  this store creates.
- Require the exact rule on the data root: rejected. Typical application-data
  directories inherit SYSTEM and Administrators grants, and automatic ACL repair
  is not allowed.
- Enable the backend in the core now: deferred. Windows capture, metadata
  preservation, and publication need their own review.
- Map missing names to `NotFound` for the workspace broker: rejected. It would
  change a model-visible error category on Windows.

## Consequences

The store is implemented and natively tested but unreachable from the product.
Windows behaviour is unchanged. The application version stays 0.9.0.

The next increment must add Windows capture, security-metadata preservation, and
publication. Observations from Windows 11 build 26300 on NTFS, using
`FileRenameInformationEx` with a retained source handle and target parent:

| Case | Result |
| --- | --- |
| Replace and POSIX semantics, target closed | Success |
| Target open with delete sharing | Success |
| Target open without delete sharing | `STATUS_SHARING_VIOLATION`, target unchanged |
| No replace flag, target exists | `STATUS_OBJECT_NAME_COLLISION`, target unchanged |
| Target read-only | `STATUS_ACCESS_DENIED`, target unchanged |
| Target is a directory | `STATUS_FILE_IS_A_DIRECTORY`, target unchanged |

These are host observations, not vendor guarantees. The information buffer must
be at least the full structure size, or the call fails with a length mismatch.

The directory-option gap in the file-access guide also applies here. Microsoft's
`NtCreateFile` page omits `FILE_OPEN_REPARSE_POINT` from its compatible directory
flags and discourages write-data access on directories, which shares a bit with
`FILE_ADD_FILE`. Native tests show acceptance on this host only.

Tests cover exact DACLs decoded independently through SDDL, broadened and
unprotected ACLs, untrusted roots, junctions, hard links, exclusive creation,
inventory bounds, and retained-handle rename blocking. A foreign owner was not
tested because setting one needs elevation. Physical power loss, ReFS, filter
drivers, and protection from same-user or administrator actors remain open.

## References

- [Replacement contract](../file-access.md#native-backend-gates)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0021](0021-store-checkpoints-with-native-private-handles.md)
- [ADR-0022](0022-publish-one-verified-stage-on-linux-ext4.md)
- [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
- [NtFlushBuffersFileEx](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntflushbuffersfileex)
- [FILE_RENAME_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
- [GetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo)
- [NtCreateFile](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile)
