# 0015: Archive operation logs without consuming settings capacity

- Status: Accepted
- Date: 2026-11-09

## Context

Operational file checks appended diagnostic events to the bounded settings log.
Repeated file calls could exhaust the capacity needed to change permissions,
including when the separate operation log was already full. The operation log
also needed a supported way to preserve records and continue after its cap.

## Decision

Keep live operational checks free of settings-log writes. The broker still
checks current policy, native scope, and binding expiry and records admitted
target operations in the separate intent/outcome journal. The explicit settings
diagnostic remains audited in control-v1.
Stop file admission when the settings log lacks capacity for a later permission
change. Otherwise an older full settings log could retain unrevocable access.

Provide trusted manual export through file rotate. Strictly validate and sync
the existing operation log, refuse unresolved operations, close the file handle,
and make one native no-replace move into an explicitly selected existing private
directory outside application and development storage. Return its exact digest,
event count, and native path. A later ordinary open starts a fresh active log.
Keep existing journal schemas and the control-store identity unchanged.

Each manual export remains bounded by the existing log cap. Export storage and
retention belong to the operator; there is no automatic accumulation, pruning,
or aggregate export quota. Existing-only archive inspection cannot append.
Native move or directory-sync uncertainty requires path inspection, never replay.
The [file-access guide](../file-access.md) owns the precise contract and limits.

## Alternatives considered

- Keep file-request diagnostics in control-v1: rejected because operation traffic
  must not consume settings capacity or prevent permission changes.
- Truncate the active operation log or discard oldest records: rejected because
  it loses audit evidence and can erase unresolved operation state.
- Automatically create unlimited archives or silently prune them: rejected
  because retention and export destinations need an explicit operator choice.
- Copy and delete on unsupported or cross-filesystem moves: rejected because it
  adds partial-copy states. Require the supported atomic native move or fail.
- Resume an unfinished workspace operation during rotation: rejected because
  journal metadata does not authorize replay or prove its side effects.

## Consequences

File traffic no longer grows the settings log. Completed operation logs can be
exported without losing their bytes. Operators must keep exports private,
manage their storage, and inspect uncertain export results. Same-user hostile
mutation, read-only archive media, and Windows directory power-loss durability
remain outside current guarantees. Settings-log rotation remains separate work.

## References

- [File access, archive and recovery](../file-access.md)
- [Workspace settings](../workspace-permissions.md)
- [Testing standards](../testing-standards.md)
