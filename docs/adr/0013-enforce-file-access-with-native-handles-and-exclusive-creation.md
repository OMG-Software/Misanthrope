# 0013: Enforce file access with native handles and exclusive creation

- Status: Accepted
- Date: 2026-11-08

## Context

Workspace preferences and bindings exist, but a diagnostic policy result does
not authorize an object opened later by pathname. Untrusted repository paths
can contain links, special files, mounts, and platform-specific aliases. File
creation can also finish ambiguously if a write or audit acknowledgement fails.

## Decision

Introduce a core-owned file broker with current binding checks, native rooted
opens, opened-object identity checks, bounded data, and private operation audit.
Keep core and CLI free of unsafe code. Use safe Rustix filesystem primitives
on Linux and narrowly reviewed Windows native wrappers in the platform crate.
Fail closed if required native controls are unavailable. Keep model tool
dispatch and process execution disabled in this increment.

Provide read, list, exclusive new-file creation, and single-directory creation.
Do not replace or delete existing entries. Persist intent before mutation and
outcome before acknowledging it; withhold read data after audit failure. An
unfinished or uncertain operation requires trusted user acknowledgement and
never triggers replay. Keep operation records separate from the control-v1
settings schema so old settings readers remain compatible.

The [file-access guide](../file-access.md) owns exact limits, recovery behavior,
native requirements, and the limits of handle-based containment.

## Alternatives considered

- Authorize a canonical path and reopen it: rejected because path substitution
  can change the object used after authorization.
- Apply diagnostic permission flags to unrestricted file I/O: rejected because
  flags and IDs cannot enforce native target containment.
- Use capability-library defaults without checking their contracts: rejected
  because fallback, mount, reparse, and enumeration behavior must match this
  broker's stricter boundary explicitly.
- Replace after hashing the current file: deferred because neither rename API
  supplies atomic expected-inode/content compare-and-swap against independent
  editors. A successful precheck does not justify a no-lost-update claim.
- Delete partial creations automatically: rejected because the name can have
  been changed by another actor. Preserve evidence and require inspection.
- Append new operation variants to control-v1: rejected because its older strict
  readers would reject the settings log. A separate journal preserves settings
  compatibility and keeps operation recovery explicit.

## Consequences

The CLI can exercise real bounded workspace I/O without exposing commands or
replacing existing user files. Interrupted creation can leave a partial entry.
Audit capacity is finite. Linux needs the required openat2 controls and procfs;
Windows needs the qualified handle-relative APIs. Neither path prevents a
concurrently hostile same-user process from moving filesystem objects or
altering authority. Native process confinement and replacement/recovery
qualification remain separate release gates.

## References

- [Workspace settings](../workspace-permissions.md)
- [Security rules](../security-coding-rules.md)
- [Linux openat2](https://man7.org/linux/man-pages/man2/openat2.2.html)
- [Linux renameat2](https://man7.org/linux/man-pages/man2/renameat.2.html)
- [Windows NtCreateFile](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile)
- [Windows file rename information](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
