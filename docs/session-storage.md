# Session storage and archiving

This document defines the planned session journal and archive behaviour. There
is no persistence implementation or archive UI yet. Conversation and session
refer to the same saved coding interaction in this contract.

## Location and file naming

Save each session in its own UTF-8 JSONL file under Misanthrope's per-user
application data directory, outside the development workspace:

```text
sessions/<year>/<month>/<date>/misanthrope-<time>-<guid>.jsonl
sessions/2026/11/08/misanthrope-12-34-56-789Z-7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470.jsonl
```

Use the session's creation time in UTC. Year has four digits; month and date
have two digits. Date means the day of the month. Time uses HH-MM-SS-mmmZ, with
milliseconds and no colon, so the filename is valid on Windows and Linux. GUID
means a randomly generated UUIDv4 in lowercase hyphenated form. Create the file
exclusively; a collision must never overwrite an existing session.

Choose the normal per-user local application data location through platform
APIs. Keep the storage root in trusted application settings. Do not derive it
from a repository instruction or model-supplied path. Storage relocation needs
a separate controlled migration with writers stopped and recovery defined.

Keep the original path when a session crosses midnight, resumes on another day,
changes title/workspace, or is archived. The date tree organizes files; the UI
tree groups sessions by their recorded workspace identity and folder. Store
lossless native paths where required; display text is never an authorization key.

This is Misanthrope's own versioned schema with the requested date-based layout.
It does not promise compatibility with ChatGPT exports or Codex internal event
schemas. Do not read or modify another application's session directory. Any
future importer needs its own explicit format and trust contract.

## JSONL event contract

Write one JSON object per line, without a byte-order mark or pretty-printed
multiline records. Escape newlines inside strings. Writers end every complete
record with LF; readers accept LF or CRLF. Each event has this envelope:

| Field | Contract |
| --- | --- |
| schema_version | Integer schema version; initially 1 |
| session_id | The UUID in the filename; constant for the journal |
| event_id | Unique UUID for this event |
| seq | Contiguous integer sequence starting at zero; authoritative order |
| timestamp | RFC 3339 UTC event time; clock changes must not reorder events |
| type | Defined event type |
| payload | Object validated against that event type's schema |

The first event is session.started and includes creation metadata, workspace
identity, and safe application/version identifiers. Record subsequent title and
workspace changes, user/assistant messages, bounded streaming updates, request
manifests, tool intents/results, compaction records, usage, errors, interruptions,
and archive/restore events. Preserve source labels and causal request/tool IDs.
Distinguish model-requested actions from core-observed execution results.

Illustrative records below show the envelope and archive state. They are schema
examples, not a transcript produced by an implemented application:

```jsonl
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"f23d5e17-ef07-40dd-8db2-6e6b4a0ce12b","seq":0,"timestamp":"2026-11-08T12:34:56.789Z","type":"session.started","payload":{"workspace_id":"workspace-1","title":"Project overview","app_version":"unreleased"}}
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"eac73dca-838e-4802-a0eb-8bafbf00cd7c","seq":1,"timestamp":"2026-11-08T12:34:57.000Z","type":"message.recorded","payload":{"role":"user","source":"user_ui","text":"Explain this project."}}
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"62349f2a-9cc5-42c1-94c4-2c7e7ae355eb","seq":2,"timestamp":"2026-11-08T12:35:03.000Z","type":"message.recorded","payload":{"role":"assistant","source":"provider","text":"This is a planned coding assistant harness."}}
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"b91ed8a6-c20a-4f98-aac0-c9ecf4984a99","seq":3,"timestamp":"2026-11-08T12:36:00.000Z","type":"session.archived","payload":{"source":"user_ui"}}
```

Before implementing the writer, define and version the complete event schemas,
limits, and migration rules. Preserve unknown data for inspection, but do not
resume a journal with unsupported required semantics or schema versions. Invalid
sequence numbers, duplicate IDs/keys, or conflicting session IDs require a
visible recovery/read-only state, not silent reinterpretation.

Store large outputs and binary attachments as bounded artifacts referenced by
safe session-local identifiers, with content digests and availability status.
Validate references within application-managed storage; never treat a transcript
path or URI as permission to read a host file or contact a service. Missing
artifacts remain visible as missing. Do not invent their content.

## Durable writes and recovery

The JSONL journal is the source of truth for transcript and archive state. A
search/tree index is derived and rebuildable. Its database format is not selected
yet. Losing or clearing a cache/index must not delete the journal. Use incremental
indexing and bounded streaming reads; do not load every transcript into memory.

Use one Rust-owned writer per session, with a bounded queue and exclusive
cross-process ownership. Multiple windows route through that owner or receive a
clear conflict. Repeated event submissions must not append duplicate IDs.
Flush buffered stream chunks at defined boundaries. Synchronize critical events
to durable storage before acknowledging session creation, archive, or restore.
Persist the intent/identity before dispatching an operation with side effects.
Specify file and directory durability guarantees for each supported OS/filesystem.

On restart, read complete valid records up to the last newline. Preserve an
incomplete tail in a recovery artifact before repairing it under exclusive
ownership. Report the recovery and record it; never silently discard evidence.
Interior corruption requires recovery/read-only handling instead of skipping
records and resuming. File replacement/migration must keep a recoverable original.

If persistence fails, report the failure and stop dispatching new mutations that
cannot be recorded. If an external action may already have occurred, retain its
uncertain status and reconcile state before retry. A persisted intent without a
result is incomplete, not permission to replay. JSONL alone cannot provide
exactly-once execution of external side effects.

## Archive, restore, and resume

Provide Archive in the session tree and conversation view. Hide archived sessions
from the active tree, while keeping an Archived view/filter with folder grouping,
search, transcript inspection, and Restore. Archive preserves the same journal,
session ID, attachments, title, and workspace association. It does not move or
delete the JSONL file, compact history, or erase provider-retained data.

For an idle session, release session-owned execution resources and MCP
connections, append session.archived durably, then update the index/UI. Repeated
archive requests are idempotent. Do not report archive complete if local cleanup
failed or remains unobserved.

For active turns/background work, offer Archive when idle or Stop and archive.
Persist a session.archive_requested event with its request ID and selected mode.
Show the pending state and allow cancellation through a recorded state change.
After restart, recover that intent without starting a new turn or replaying work.
Archive when idle blocks new turns and unrelated background tasks but permits
bounded work needed to finish the existing turn. Stop and archive blocks further
tool dispatch and cancels/observes affected local tasks immediately.

Both modes release session-scoped MCP connections and execution resources before
the archive event is committed. Do not affect other sessions. Unknown remote
outcomes remain explicitly recorded; do not claim that cancellation rolled back
remote effects.

Restore appends session.restored durably and returns the session to its workspace
group. Restore is also idempotent and starts no execution. Resuming requires a
separate user action. Revalidate the current workspace/root, provider/account,
permissions, enabled skills/MCP servers, and instruction revisions. A missing,
moved, or disallowed workspace needs explicit resolution. Old grants, paths,
tool calls, or connection records must never restart work by transcript replay.

Archived sessions remain retained by default. Show their storage use separately
from caches. Permanent deletion, export, and retention cleanup are distinct
actions with explicit scope; archiving and cache clearing cannot imply deletion.

User-saved [memories](memories.md) have their own store and lifecycle. Archive or
cache cleanup does not delete them. Record selected memory IDs/revisions with
request manifests, but never recreate deleted memories or restore old scope
choices by replaying a journal. Historical snapshots follow journal retention;
memory deletion does not promise removal of every historical copy.

## Privacy and trust

Keep journals, indexes, recovery artifacts, and attachments outside every task's
writable view. Deny indirect access through links and overlapping roots. Use
per-user file protections and safe core APIs; the frontend does not choose raw
journal paths. JSONL is readable text, not encryption or tamper evidence.

Never persist authentication tokens, passwords, authorization headers, login
codes, or secret-bearing environment/arguments as application diagnostics. Use
safe credential references and redact known sensitive fields before queuing an
event. User messages and tool output can still contain sensitive project data;
do not claim automatic detection of every secret. Protect backups and exports
under their own explicit policy.

Validate journals as untrusted input when reading. A role label, approval event,
or recorded policy snapshot cannot grant current authority. Use current
core-owned policy and operation records for authorization/reconciliation. Safely
render all message/tool content. Preserve the provenance and invalidation rules
from [agent instructions](agent-instructions.md) and
[context management](context-management.md).

## Acceptance requirements

Test Windows/Linux naming, UTC midnight and timezone changes, clock rollback,
UUID collisions, Unicode/native paths, stream reconstruction, malformed/large
records, unknown schemas, duplicate IDs/keys, multi-writer contention, interrupted
append, disk full, interrupted index updates, index rebuild, missing artifacts,
link/path attacks, redaction, archive/restore idempotency, active-task shutdown,
MCP disconnection, crash recovery, changed workspace/permissions on resume, and
absence of tool/remote side-effect replay. Validate the documented JSONL examples
as examples only; they do not establish runtime persistence guarantees.

## Reference boundary

[OpenAI's developer command documentation](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
describes archive/restore without deleting a saved transcript. This is a useful
behavioural reference. The exact path, event schema, and durability requirements
above are Misanthrope decisions, not a claimed OpenAI storage-format contract.
