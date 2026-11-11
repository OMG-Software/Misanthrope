# 0025: Restore and delete checkpoints explicitly

- Status: Accepted
- Date: 2026-11-11

## Context

ADR-0022 and ADR-0024 enabled trusted apply on Linux ext4 and Windows NTFS.
Each apply retains a private checkpoint holding the original and proposed bytes.
Before this decision the CLI could only inspect and acknowledge them. Restore
and deletion were unavailable. Retained checkpoints were capped at 64, and no
route freed a slot. ADR-0024 left a retained stage that could carry the source
DACL after a crash. It chose to keep inspection and acknowledgement read-only,
so recovery could not remove that stage.

Restore can lose work if it ignores later edits. Deletion destroys evidence
that later recovery may need. Both must remain explicit trusted-operator
actions, and neither may weaken the replacement contract.

## Decision

Add two trusted CLI commands.

`file restore` writes one retained checkpoint's `original` bytes back to the
same workspace target. It runs the normal apply path with those bytes as the
proposal. The operator supplies the current file's expected digest and byte
count and acknowledges that writers are stopped. Restore therefore needs fresh
read/write authority, the native object checks, and a new checkpoint that keeps
the bytes it replaces. The core rereads the original payload with the store
open read-only. It checks the recorded digest and UTF-8, then closes the store
before apply begins. Restore refuses checkpoints for another workspace or
native path, deleted checkpoints, and any state with a pending operation or
deletion. A missing target is refused; there is no restore-as-create. The
ledger records restore as an ordinary replacement and does not link it to its
source checkpoint.

`file delete-checkpoint --permanent` removes one resolved, retained
checkpoint's files. It requires no pending operation. An uncertain outcome must
be acknowledged first. Deletion runs in three steps:

1. Append and sync `checkpoint_deletion_started`. From here, other file work is
   refused until the deletion finishes.
2. Remove only the fixed names `stage`, `proposed`, `original`, and
   `manifest.json`, then the entry directory. Each removal pins one native entry
   and refuses links, hard links, and substitution. Directory removal is not
   recursive, so an unknown name stops it.
3. Append and sync `checkpoint_deleted`.

The operation's history stays in the ledger and in inspection output, marked
`deleted`. A deleted operation no longer counts toward the 64 retained
checkpoints. If deletion is interrupted, only the same request can finish it.
That request reports `resumed: true`. A failure after the intent poisons the
writer and reports an uncertain result.

On Linux, removal pins the name with `openat2` `O_PATH`. It then checks for a
regular single-link file on the same device, calls `unlinkat`, and requires the
pinned inode to report no links. On Windows, removal opens the name relative to
its parent with delete access. It validates the handle and sets
`FileDispositionInfoEx` with delete, POSIX semantics, and ignore-read-only. After
the handle closes, it checks that the name is gone.

The ledger keeps schema version 1. Version 0.10.0 refuses the two new actions
as unknown, so it fails closed on a ledger containing a deletion. A ledger with
restores but no deletions remains readable by 0.10.0.

## Alternatives considered

- Link restore records to their source checkpoint in the ledger: rejected. A new
  field would make ledgers that only contain restores unreadable by 0.10.0. The
  receipt still reports `restored_from`.
- Raise the ledger schema version: rejected. The new actions already fail closed
  in older binaries, and existing records are unchanged.
- Delete the entry directory recursively: rejected. A recursive delete could
  remove unknown or substituted objects. Fixed names and a non-recursive
  directory removal leave anything unexpected in place and report it.
- Delete or evict checkpoints automatically when the retention cap or quota is
  reached: rejected. The replacement contract forbids evicting evidence.
- Delete through the acknowledgement command, or let recovery remove a retained
  stage: rejected. ADR-0024 keeps inspection and acknowledgement read-only.
  Deletion is a separate, explicit command.
- Repair ACLs or clear attributes so that removal succeeds: rejected. OMGCode
  does not repair security state. A refused removal is reported and retried by
  the operator.
- Exempt deletion records from the storage quota: rejected. The ledger bounds
  stay uniform. A store near its quota may need its intent space before a
  deletion can start, as in 0.10.0.

## Consequences

Version 0.11.0 can restore an earlier version of a file and free retained slots
without losing history. Deletion also gives the operator an explicit way to
remove a retained stage that ADR-0024 left carrying the source DACL. Inspection
and acknowledgement stay read-only.

Neither command is power-loss qualified. Restore inherits each platform's apply
claim, and Windows makes no power-loss claim. Deletion has not been tested
against power loss on either platform.

Limits and gaps:

- Restore is a precondition check followed by a rename, not compare-and-swap.
  The operator must stop independent writers, as for apply.
- The ledger's 4,096-record cap bounds the store's lifetime. A successful apply
  or restore uses five records, an acknowledgement adds one, and a deletion
  uses two. No command compacts the ledger.
- On Windows, a read-only file whose DACL denies the owner write-attributes
  access cannot be removed. Apply refuses read-only sources, so only
  out-of-band changes can create this state. The removal is refused and the file
  is kept.
- A Windows private-directory handle denies delete sharing. Any open handle to
  the entry blocks its removal.
- The ledger does not record which checkpoint a restore came from.
- Same-user and administrator actors remain outside this boundary.

## References

- [Phase A4 restore and deletion](../file-access.md#implemented-restore-and-checkpoint-deletion-phase-a4)
- [Replacement contract](../file-access.md#replacement-contract)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0022](0022-publish-one-verified-stage-on-linux-ext4.md)
- [ADR-0024](0024-publish-one-verified-stage-on-windows-ntfs.md)
- [FILE_DISPOSITION_INFO_EX](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_disposition_info_ex)
- [openat2(2)](https://man7.org/linux/man-pages/man2/openat2.2.html)
