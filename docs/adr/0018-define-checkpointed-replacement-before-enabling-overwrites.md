# 0018: Define checkpointed replacement before enabling overwrites

- Status: Accepted
- Date: 2026-11-09

## Context

PR #9 added read-only edit previews. They contain exact digests but do not lock
the target, grant write authority, or preserve its previous bytes. ADR-0017 and
the implementation plan require a separate reviewed replacement contract before
any overwrite is enabled. The existing broker supports exclusive creation;
its native APIs, strict v1 operation log, and recovery acknowledgement do not
implement replacement.

A hash check followed by rename is not atomic compare-and-swap. An independent
editor can change the target during capture or after the final check. Native
namespace publication and content-version checks are separate properties.
Checkpoint retention, security metadata, audit compatibility, and uncertain
outcomes therefore need explicit decisions before runtime work.

## Decision

Accept the [replacement contract](../file-access.md#replacement-contract)
through [PR #10](https://github.com/OMG-Software/OrangeCode/pull/10), merged on
2026-11-09. Phase A2 implements this contract in bounded increments. Acceptance
does not establish that native storage, replacement, or recovery is available.

The first replacement interface is trusted CLI only. Require fresh read/write
authority, bounded exact expected bytes, native identity checks, and a durable
private checkpoint before publication. Detect observed conflicts without
changing workspace entries. Require the operator to stop independent writers
and state that this is an operating assumption, not enforced isolation or a
no-lost-update guarantee. Captured bytes are not an atomic filesystem snapshot.

Keep original and proposed payloads plus a separate staging file in private
application storage outside the development root, on the target filesystem.
Reject cross-filesystem configurations. Use exclusive operation names and a
separately versioned replacement ledger; leave strict existing journal formats
unchanged. Bound entries, total storage, payloads, manifests, and ledger
records. Reserve terminal capacity. Retain all evidence without automatic
eviction or cleanup; refuse new replacements at the cap.

Distinguish unused installations from missing initialized storage with a
separate bounded initialization journal outside the checkpoint directory.
Ordinary reads do not provision replacement state. Persist an initializing
anchor before exclusive store creation and mark it ready only after its ledger
and directory entries are synced. Partial or inconsistent provisioning requires
inspection; a missing initialized ledger is never automatically recreated.

Use reviewed native handle-relative publication only. Linux `renameat2` and
Windows retained-handle rename are directions for implementation review, not
qualified OrangeCode backends. Checkpoint persistence, security-metadata
preservation, and Windows ACL/directory durability gates must pass before
availability. Preserve and report uncertainty after a possible commit. Never
retry, roll back, delete artifacts, or restore automatically. A future explicit
restore must validate current content and checkpoint it through the same path.

The file-access guide owns the exact limits, state sequence, platform gates,
and recovery rules. The [testing standards](../testing-standards.md#replacement-acceptance-gates-planned)
own acceptance evidence. Contract review must resolve disagreements with these
requirements before Phase A2 enables replacement.

## Alternatives considered

- Enable apply with the preview digest alone: rejected because preview is a
  snapshot and neither permission nor an exclusive content-version token.
- Claim that a second hash or an advisory lock makes rename a CAS: rejected
  because independent writers need not cooperate.
- Stage in the workspace to support a different checkpoint filesystem:
  rejected for this first contract. Private same-filesystem staging avoids
  workspace preparation entries and permits one native publication primitive.
  The resulting filesystem restriction is explicit.
- Truncate in place, copy/delete, or use path-based replacement as a fallback:
  rejected because those routes add partial writes or weaken the native path
  boundary. Unsupported requirements must refuse replacement.
- Add replacement events to strict v1 journals: rejected because older readers
  reject unknown variants. A separate versioned ledger preserves those formats,
  but older binaries cannot enforce its recovery rules. Downgrade while
  replacement state exists is unsupported.
- Evict checkpoints at the quota or automatically undo uncertain work: rejected
  because this can discard recovery evidence or overwrite later user edits.
  Retention/export/deletion needs a separate reviewed lifecycle.

## Consequences

The next runtime increment has a reviewable contract and explicit native test
gates. Replacement remains disabled until implementation and native verification
finish. Phase A remains incomplete. The original Phase A1 change was
documentation-only and required no version bump.

Availability is deliberately restricted. Application storage and the target
must share a filesystem. Full storage stops replacements. Independent writes
remain a data-loss risk after the last check, and a checkpoint cannot preserve
every concurrent version. Windows backend qualification remains open. These
limits must be visible to the operator; tests cannot remove them.

## References

- [Implementation plan](../../IMPLEMENTATION_PLAN.md)
- [Workspace file access](../file-access.md#replacement-contract)
- [Security rules](../security-coding-rules.md)
- [Platform support](../platform-support.md)
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
- [ADR-0017](0017-preview-edits-before-enabling-replacement.md)
- [Linux rename](https://man7.org/linux/man-pages/man2/rename.2.html)
- [Linux fsync](https://man7.org/linux/man-pages/man2/fsync.2.html)
- [Windows rename information](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
- [Windows ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)
