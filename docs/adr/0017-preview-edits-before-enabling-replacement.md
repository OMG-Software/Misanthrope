# 0017: Preview edits before enabling replacement

- Status: Accepted
- Date: 2026-11-09

## Context

Safe editing needs diff proposals, conflict detection, checkpoints, and
recovery. The broker deliberately supports exclusive creation only. Existing
file replacement is harder: a content-hash check followed by rename is not
atomic compare-and-swap against an independent editor, and Windows and Linux
replacement semantics need separate native review. Enabling overwrites before
those contracts exist would create a data-loss boundary that tests cannot
justify.

A read-only preview can provide reviewable change evidence without taking on
the replacement contract. It also gives the operator a bounded way to compare a
workspace file with proposed content while the only write operation remains
exclusive creation.

## Decision

Implement `file diff` as a bounded, read-only preview. It requires the same live
session binding and read preference as a broker read. The current file is read
through the existing broker, including its normal operation record. Proposed
content comes from a trusted operator-selected local regular file, opened
read-only without following a final link. Windows reparse, device, and
non-regular handles are rejected. Both sides must be UTF-8 and at most 1 MiB,
with at most 65,536 lines per side when a diff is needed. The result contains
exact current and proposed SHA-256 digests and byte counts plus a single-hunk
diff around the common line prefix and suffix, bounded to 64 KiB. A missing
final newline is marked explicitly.

The preview never writes to the workspace, creates no proposal or checkpoint
record, and is not a write grant. Existing-file replacement remains `false`, and
the model tool set remains read/list only. The preview digest is a snapshot of
the bytes read, not a lock or a conflict-free replacement token. A later
replacement operation must recheck the target identity and content, define
checkpoint retention and conflict handling, and account for uncertain outcomes
without automatic retry.

The [file-access contract](../file-access.md) owns the preview bounds and
operational details. [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
owns the existing-creation and non-CAS replacement limitation.

## Alternatives considered

- Implement in-place replacement with a hash precondition now: rejected because
  hash-check-then-rename is not atomic compare-and-swap, platform replacement
  semantics are unreviewed, and checkpoint/recovery behaviour is undefined.
- Add a proposal and checkpoint store before any preview: deferred because the
  preview can provide useful review evidence without introducing another
  private journal and retention policy. Checkpoints belong with the replacement
  contract.
- Add a third-party diff dependency: rejected for this increment because a
  single common-prefix/suffix hunk is sufficient for a bounded preview and avoids
  a new dependency and parser surface.
- Expose the preview as a model tool: rejected. The preview is trusted CLI
  evidence, and model dispatch remains read/list only.

## Consequences

Operators can review the bounded diff and exact digests without granting write
authority. A truncated diff can omit changes, including proposed lines. The
preview is not a minimal diff and can show a larger changed region than a
line-optimal algorithm. The line cap rejects pathologically
line-dense files rather than allocating an unbounded line index. The preview
does not detect an independent edit made after the broker read, and it does not
provide checkpoints or recovery. Actual
replacement, conflict resolution, and checkpoint retention remain future work
behind a separate reviewed contract.

## References

- [Workspace file access](../file-access.md)
- [Security coding rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md)
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
