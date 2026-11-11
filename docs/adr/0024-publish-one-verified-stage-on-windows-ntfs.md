# 0024: Publish one verified stage on Windows NTFS

- Status: Accepted
- Date: 2026-11-11

## Context

ADR-0022 enabled trusted apply on Linux ext4. ADR-0023 added a Windows NTFS
private store that the core did not use. It deferred Windows capture,
security-metadata preservation, and publication to a separate review. Windows has
no equivalent of a Unix mode, so the replacement record needs a different
security representation. A stage is a new file, so anything the source carries
must be copied exactly or refused.

## Decision

Enable trusted apply and checkpoint storage on Windows NTFS. `PrivateDirectory`
now selects the ADR-0023 backend, and the core's refusal is lifted. Requirements
match ADR-0022 where the platforms allow: an exact precondition, a stopped-writers
acknowledgement, a private stage, a durable commit intent, one native attempt, and
ordered flushes and outcomes.

Record Windows security as canonical SDDL holding the owner, group, DACL, and
DACL control flags, plus a `not_content_indexed` flag. The manifest gains a
`Windows` variant. The schema version stays 1 and Linux records are unchanged.

Capture through one retained source handle opened read-only, sharing only read
and delete. A writer that already has the file open refuses the capture, and new
writers are blocked while the handle is retained. An in-place edit therefore
cannot happen during the operation. A replacement target also needs a transient
delete-access open, which refuses a reader that does not share delete. Without
that check the rename would fail only after the commit intent.

Refuse a source that a stage cannot reproduce:

- attributes other than archive, normal, and not-content-indexed;
- alternate data streams and extended attributes;
- mandatory labels, resource attributes, and scoped policy entries;
- an owner other than the current user or the token's default owner;
- a name that differs from the stored name, including case and 8.3 aliases;
- hard links, reparse points, a different volume, and non-NTFS filesystems.

Apply the descriptor to the private stage with `SetKernelObjectSecurity`, then
read it back and compare it exactly. That comparison, not the call, is what
establishes the result. `SetSecurityInfo` is not used because it recomputes
inheritance and would change inherited entries.

Publish with one `FileRenameInformationEx` call using replace-if-exists and POSIX
semantics, from the retained stage handle to the retained target parent. Flush
the published file and both parents, following ADR-0023. Failures after the rename
stay uncertain and are never retried.

Before the commit intent and again before the rename, recheck the retained
source, the parent, and whatever the name now refers to. The recheck covers
identity, times, length, attributes, exact name, streams, security, and bytes.

## Alternatives considered

- `ReplaceFileW`: rejected by the replacement contract. It is path-based and its
  documented errors include partial name and metadata changes.
- Refuse the not-content-indexed attribute: rejected. It is a user preference and
  is commonly inherited across a whole development folder. It is preserved.
- Rely on the retained handle's change time alone: rejected. A caller with
  write-attributes access can restore file times. Security, streams, name, and
  identity are each compared directly.
- Preserve the audit SACL: not possible without `SeSecurityPrivilege`, which an
  unelevated process does not have. Recorded as a gap below.
- Compare-and-swap semantics: not available. A swap or removal after the final
  check remains undetected, as on Linux.

## Consequences

Version 0.10.0 can replace an existing file on Windows NTFS through trusted CLI
apply. Restore, deletion, and model file tools remain unavailable.

**Windows makes no power-loss claim and stays unqualified for release.** ADR-0023
applies: Microsoft gives no contract that a directory-handle flush persists an
entry or rename.

Observations from Windows 11 build 26300 on NTFS. These are host behaviour, not
vendor guarantees:

| Behaviour | Result |
| --- | --- |
| Descriptor-to-SDDL reported length | Can exceed the text, so conversion stops at the first NUL |
| `SetKernelObjectSecurity` with `SE_DACL_AUTO_INHERITED` only | The flag is dropped unless `SE_DACL_AUTO_INHERIT_REQ` is also set |
| `SetKernelObjectSecurity` protected state | Follows the descriptor's control bits; `PROTECTED_` and `UNPROTECTED_DACL_SECURITY_INFORMATION` had no observable effect, so they are not passed |
| `FileEaInformation` | Works with read access, without `FILE_READ_EA` |
| Empty-name relative open of a parent | Works with `FILE_ADD_FILE` |

The data-root rule from ADR-0023 now affects users directly. A data folder that
grants any principal beyond the current user, SYSTEM, and Administrators is
refused, including through inherited grants. The operator must remove the grant
or choose a private `--data-dir`. OMGCode does not repair ACLs.

Gaps that remain open:

- Audit SACLs cannot be observed unelevated and are not preserved.
- A foreign owner was not tested, because setting one needs elevation.
- Cross-volume and non-NTFS refusals are unit tested for the comparison only, not
  on real second volumes.
- Physical power loss, ReFS, and filter drivers are untested.
- Same-user and administrator actors are outside this boundary.
- The directory-option gap recorded in ADR-0023 still applies.

## References

- [Replacement contract](../file-access.md#native-backend-gates)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0022](0022-publish-one-verified-stage-on-linux-ext4.md)
- [ADR-0023](0023-store-windows-checkpoints-with-verified-acls.md)
- [FILE_RENAME_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
- [SetKernelObjectSecurity](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-setkernelobjectsecurity)
- [SECURITY_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/secauthz/security-information)
