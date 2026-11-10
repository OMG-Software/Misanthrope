# 0022: Publish one verified stage on Linux ext4

- Status: Accepted
- Date: 2026-11-10

## Context

ADR-0018 requires verified checkpoints, exact preconditions, native publication,
and explicit uncertainty. ADR-0021 provides the protected store. Its same-device
check does not establish rename compatibility across mount aliases. Its
standalone preparation stops before staging. Windows ACL and directory sync
qualification is still missing.

## Decision

Enable trusted CLI apply only for the tested Linux ext4 backend. Require
`openat2`, procfs, and a positive `STATX_MNT_ID` result. This requires Linux
5.8 or later and leaves ordinary broker requirements unchanged. Match retained
parent mount IDs and verify the filesystem type in bounded kernel mountinfo.
Probe directory sync before writing private replacement artifacts. Refuse other
filesystems and missing native controls. The capability flag describes the
implementation; it cannot promise that a particular apply can proceed.

Require a live binding, both preferences, exact current SHA-256 and size, and
complete bounded UTF-8 input. Require the operator to stop independent writers.
The flag confirms that assumption. The native check and rename remain separate;
an intervening edit, entry swap, or removal can go undetected.

Capture through the retained source handle. Require ownership by the effective
user. Preserve ordinary mode, owner, and group on the exclusive private stage.
Refuse extended attributes, including access ACLs, special mode bits, unsupported
inode flags, project IDs, and extent hints. Permit kernel-managed allocation
extents and changed inode ID and timestamps. Verify exact supported metadata and
bytes both before and after publication. Preserve the original and proposed
payloads separately from the stage.

Use safe Rustix calls for native opening, rename, metadata changes, and sync.
`statx` does not report every ext4 attribute. Use the safe inode flag query and
one narrow read-only `FS_IOC_FSGETXATTR` wrapper to detect project and extent
metadata that Rustix does not otherwise expose. The wrapper uses a fixed Linux
UAPI buffer on the same regular file handle. No raw handle or pointer leaves it.

Keep the control writer and checkpoint anchor lock throughout publication.
Record and sync intent, prepared checkpoint, and prepared stage in order.
Reserve the whole remaining ledger tail and maximum stage bytes before work.
Observe parent, target, and stage again before syncing `commit_intended`.
Repeat the native checks after that sync and check live authority immediately
before one `renameat2` call. Verify the published object and absent stage name.
Sync the published file, private source parent, and target parent before the
success outcome. A detected pre-intent conflict retains its private evidence.

Any failed commit, verification, sync, or outcome append leaves uncertainty.
Preserve the visible history and never retry or swap back. A complete visible
outcome after a failed sync can be inspected without claiming the failed call
succeeded. Acknowledgement clears a blocker without executing or authorizing
work. The existing ledger schema already represents these transitions.

## Alternatives considered

- Use equal device IDs alone: rejected. Two mounts of the same filesystem can
  refuse cross-mount rename. Match mount IDs and verify native controls.
- Support every Linux filesystem with `renameat2`: rejected for this prototype.
  Native tests exercise ext4. Other filesystems need separate qualification.
- Ignore ACLs, inode flags, or project metadata: rejected. Refuse metadata that
  cannot be preserved. Checking only `statx` would miss some ext4 attributes.
- Use a copy, truncate, or exchange with rollback: rejected. These alternatives
  change failure semantics, and rollback can overwrite independent edits.
- Use an expected hash as an atomic edit token: rejected. The kernel rename
  operation cannot compare it. Keep the stop-writers requirement explicit.
- Enable Windows apply with inherited ACLs or a directory-sync no-op: rejected.
  That does not meet the private store and publication contracts.

## Consequences

Version 0.9.0 adds the Linux ext4 apply path and retains standalone preparation
and existing-only recovery. There is no restore, deletion, or model dispatch.
There is no journal migration or dependency version change. The private storage
limits still include all retained intents and partial stages.

Native tests run on Ubuntu 24.04 under WSL, kernel 6.6.87.2, ext4 `data=ordered`.
Windows tests verify refusal. These results cover syscall behavior and injected
failure handling; they do not qualify physical power loss, a release filesystem
matrix, or protection against an independent same-user writer.

## References

- [Native apply contract](../file-access.md#implemented-native-apply-phase-a2c)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0021](0021-store-checkpoints-with-native-private-handles.md)
- [Linux rename](https://man7.org/linux/man-pages/man2/rename.2.html) and [fsync](https://man7.org/linux/man-pages/man2/fsync.2.html)
- [Linux statx](https://man7.org/linux/man-pages/man2/statx.2.html) and [mountinfo](https://man7.org/linux/man-pages/man5/proc_pid_mountinfo.5.html)
- [Linux inode flags](https://man7.org/linux/man-pages/man2/ioctl_iflags.2.html) and [filesystem UAPI](https://github.com/torvalds/linux/blob/v6.6/include/uapi/linux/fs.h)
