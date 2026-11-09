# 0007: Store session journals as dated JSONL

- Status: Accepted
- Date: 2026-11-08

## Context

The user requires session archiving and JSONL persistence at
sessions/year/month/date/orangecode-time-guid.jsonl. Persistence must preserve
history across compaction, crashes, archive/restore, and provider changes without
replaying side effects or restoring obsolete permissions.

## Decision

Use one versioned JSONL event journal per session in per-user application data.
Derive the date/time path from UTC creation time, use Windows-safe time text and
a UUID, and keep that path stable. Use a rebuildable index for the folder tree
and search. See the [session storage contract](../session-storage.md).

Archive/restore append durable state events in the same journal. Archive hides
the session from active lists and retains its transcript; restore does not start
work. Stop local session work or wait until idle before committing archive state.
Revalidate current settings on resume; historical records are not authorization.

## Alternatives considered

- Database-only or binary transcripts do not satisfy the requested JSONL files.
  A database may index journals, but cannot replace their transcript authority.
- Moving journals into a separate archive tree adds file/index coordination and
  changes stable paths. Keep archive state in the journal.
- Local-time paths can change meaning around timezone/daylight-saving changes.
  Use UTC for storage and local time for display where useful.
- Copying another product's internal event schema creates an unverified
  compatibility dependency. Define OrangeCode's own schema and version it.
- Replaying recorded actions on resume risks duplicate mutations and stale
  approvals. Reconstruct context and reconcile state without executing history.

## Consequences

Readable journals need access controls, bounded parsing, redaction, recovery,
exclusive writers, and explicit retention. JSONL is not encryption, a tamper-proof
audit trail, or an exactly-once execution mechanism. The exact event schemas and
platform durability guarantees must be defined and tested before implementation
is considered ready. No runtime verification exists yet.

## References

- [Context management](../context-management.md)
- [Security coding rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md)
