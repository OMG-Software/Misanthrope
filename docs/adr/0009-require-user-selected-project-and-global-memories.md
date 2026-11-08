# 0009: Require user-selected project and global memories

- Status: Accepted
- Date: 2026-11-08

## Context

The user requires both per-project and global memories, with the user choosing
where each memory is saved. Persistence and retrieval could otherwise turn
untrusted content into standing instructions or disclose one project's facts
in unrelated work.

## Decision

Provide explicit user-controlled saving with This project and Global scopes.
Bind project entries to validated project identities and keep global entries
within the current user's installation. Store both through a core-owned service
outside task-writable storage, with visible review/edit/delete/disable controls.

Load selected relevant entries as labelled supporting context under the
compiled base, explicit task, saved custom instructions, and current permissions.
Preserve source trust and record memory IDs/revisions with request manifests.
Invalidate dependent context after edits, withdrawal, scope changes, or project
changes. See the [memory contract](../memories.md).

## Alternatives considered

- Automatically selecting global scope can expose project-specific information
  without the user's choice. Require an explicit save scope.
- Saving every model-inferred fact creates persistence through untrusted output
  and unnecessary noise. Start with user-controlled entries.
- Workspace-owned memory files allow project code to modify standing context.
  Use protected application storage and a trusted save flow.
- Putting memories into the privileged base gives recalled text inappropriate
  authority. Keep memories source-labelled and subordinate.
- Deriving project identity from a shared remote URL or path prefix can connect
  unrelated copies/projects. Use a validated project registry.

## Consequences

The UI needs scope selection and memory-management controls. Retrieval adds
context tokens; selection and invalidation must be bounded and observable.
Global entries are intentionally eligible across projects, while project data
and conversation caches remain isolated. Deleting a memory cannot erase every
historical/provider copy or prevent the same fact being learned from another
source. No memory implementation or runtime verification exists yet.

## References

- [Agent instructions](../agent-instructions.md)
- [Context management](../context-management.md)
- [Session storage](../session-storage.md)
- [Security coding rules](../security-coding-rules.md)
