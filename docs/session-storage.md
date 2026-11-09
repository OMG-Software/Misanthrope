# Session storage and archiving

This document separates the current JSONL session prototype from the complete
release contract. Conversation and session refer to the same saved coding
interaction. The prototype stores a bounded transcript and archive state. A
separate journal records metadata for bounded file-broker operations. The
prototype does not provide a session UI, search index, task execution, or
recovery UI.

## Implemented prototype (v0.3.0)

The Rust core creates and opens one UTF-8 JSONL journal per session. The CLI
uses the operating system's per-user local application data location by default;
`--data-dir` is an explicit override. The default is outside the workspace, but
configured development roots reject overlap with application storage. Without a
configured root, legacy text-only sessions retain the previous unchecked
`--data-dir` override. No tool access is granted by that compatibility mode.
The journal path stays fixed
after creation. The filename contains the UTC creation time and session UUIDv4.
Creation uses exclusive file creation.

The journal holds an exclusive file lock for the lifetime of its writer. A
second writer receives a conflict. The prototype syncs every complete appended
event with `sync_all` before it updates the in-memory snapshot or reports the
append as successful. A write or sync error poisons that writer; it accepts no
further mutations. A complete event may still have reached storage when sync
reports an error, so the result can be uncertain. Reopen validates the journal
and does not retry recorded work.

Unix creation requests mode 0700 for new session directories and 0600 for a new
journal file. Existing directory permissions are not changed. Windows uses the
inherited directory ACL; explicit per-user ACL setup is not implemented. The
platform helper inspects an open journal handle for a regular, non-reparse,
single-link file. This does not contain parent-directory races or establish
workspace access confinement. Directory crash durability and Windows ACL
behavior have not been qualified. The implementation does not claim equivalent
crash durability on Windows and Unix.

Windows creation and reopening omit delete sharing, so a journal cannot be
renamed or replaced while its writer holds the file handle. This is separate
from the exclusive writer lock and the existing link checks.

The v1 prototype has this exact event subset. Every event also has the common
envelope described below. Payload objects reject unknown fields.

| Event type | Payload in v1 | State rule |
| --- | --- | --- |
| `session.started` | `title`, `workspace`, `workspace_id`, `app_version` | First event, sequence 0. `workspace_id` is the stable registry ID for new sessions with configured workspace settings; legacy/unconfigured sessions use a per-session association ID. `app_version` is the package version. |
| `message.recorded` | `role`, `source`, `text` | `user` must pair with `user_cli`; `assistant` must pair with `provider`. The source label is metadata and grants no authority. |
| `turn.started` | `request_id` | Requires an active, unarchived session with no active turn. |
| `request.manifest` | `request_id`, `manifest` | Must match the active turn. The manifest has `base_prompt`, `prompt_version`, `prompt_sha256`, `custom_instructions`, `model`, `base_url`, `context_tokens`, and `max_output_tokens`. It is historical metadata, not policy or permission. |
| `turn.completed` | `request_id` | Must match the active turn. |
| `turn.failed` | `request_id`, `code` | Must match the active turn. The code is nonempty, at most 128 UTF-8 bytes, and has no control characters. |
| `session.archived` | Empty object `{}` | Requires no active turn. Repeating the API operation is idempotent. |
| `session.restored` | Empty object `{}` | Repeating the API operation is idempotent. Restore starts no work. |

`workspace` is a tagged native path: `{"encoding":"unix_bytes","units":[...]}`
stores Unix path bytes; `{"encoding":"windows_utf16","units":[...]}` stores
Windows UTF-16 code units. It is descriptive metadata only. It is not a current
workspace authorization. The registry supplies stable IDs for configured new
sessions; the journal cannot restore or mint authority. Existing journals are
not rewritten. See [workspace settings](workspace-permissions.md).

The CLI supports `session new`, `inspect`, `archive`, `restore`, and
`interrupt JOURNAL`. Interrupt marks a recorded active turn failed with the code
`operator_interrupted`; it does not retry the model request. The current Archive
operation refuses an active turn. The operator can interrupt it first, then
archive it. There are no session-owned processes or MCP connections to clean up
in this prototype. `chat --journal` does not resume an active turn; it requires
an explicit interrupt first.

Ordinary journals are limited to 8 MiB and 10,000 events. Each event is at most
64 KiB, a message is at most 16 KiB, and a title is 1 to 256 UTF-8 bytes. Both
decoded and encoded limits apply. Active turns reserve one terminal event and
1 KiB for it. CLI chat preflights the encoded user message and manifest, plus
capacity for the reply and termination, before recording the turn marker.
The marker precedes the user message. Deterministic reply-size failures close
the turn; uncertain I/O failures poison the writer and retain recovery state.

A legacy journal already full with an active turn may append one terminal
recovery event of at most 1 KiB beyond ordinary quotas. The reader accepts at
most 8 MiB plus 1 KiB and 10,001 events, and permits overflow only for that
single matching terminal record. Ordinary records cannot use recovery capacity.
A full journal remains inspectable; it needs a new session for further chat
or other ordinary mutations. No event is discarded or rewritten to make room.
See [ADR-0011](adr/0011-reserve-terminal-journal-capacity.md).

The reader requires a final newline, accepts
LF and CRLF, and rejects a BOM, duplicate JSON keys, unknown schema versions,
unknown events, unknown fields, repeated event IDs, invalid transitions,
sequence gaps, and a session ID that conflicts with the filename. It validates
the complete journal before returning a writable handle. A malformed journal
or incomplete tail is rejected without mutation. There is no recovery or
read-only inspection UI yet.

The request manifest does not include usage or `finish_reason`. The CLI returns
those provider fields in its JSON output, but does not append them to the
journal. The prototype has no streaming, artifact, index, compaction, migration,
or memory events. The CLI redacts the currently configured LM Studio token from
new conversation text, preferences, titles, and replies before retention and
output. It redacts that same token from historical context before inference.
There is no general secret detector or automatic cleanup of older stored text.
The token itself is never intentionally added to the request manifest.

CLI results carry a lossless native journal locator as well as separate display
text. The journal string is null for paths that are not Unicode. Pass the
journal_native JSON to --journal-native when reopening on the same OS.
Post-open chat errors include this locator under error.session.

These are implemented prototype facts, not a claim that the full release
contract below is complete.

## File-operation journal

The workspace file broker records operation intent and outcome in a separate
`file-operations-v1.jsonl` file under the private application data directory.
It does not change the session JSONL event schema or the `control-v1.jsonl`
settings journal. Existing text-only sessions remain readable. A session journal's
workspace path or ID never creates a current binding or grants file access.

The file-operation log stores operation metadata, including native relative
paths, but never file contents. Treat path metadata as private user data. A
read result is returned only after its outcome is synced. A create that may have
changed the filesystem but lacks a durable outcome remains unresolved. The
operator must inspect the file and audit record, then explicitly acknowledge
inspection. Acknowledgement does not retry, repair, or remove the file. See the
[workspace file-access contract](file-access.md) for native operations and
recovery limits.

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

For release, choose the normal per-user local application data location through
platform APIs. Keep the storage root in trusted application settings. Do not
derive it from a repository instruction or model-supplied path. Storage
relocation needs a separate controlled migration with writers stopped and
recovery defined. The prototype uses this default in the CLI but also exposes
`--data-dir` for an explicit caller-selected override.

Keep the original path when a session crosses midnight, resumes on another day,
changes title/workspace, or is archived. The date tree organizes files; the UI
tree groups sessions by their recorded workspace identity and folder. Store
lossless native paths where required; display text is never an authorization key.

This is Misanthrope's own versioned schema with the requested date-based layout.
It does not promise compatibility with ChatGPT exports or Codex internal event
schemas. Do not read or modify another application's session directory. Any
future importer needs its own explicit format and trust contract.

## Release JSONL event contract

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

The release schema must define the first `session.started` event and every
supported event's payload, size limits, state transitions, and migration rules
before release. The prototype supports only the v1 event subset listed above.
Release requirements include title and workspace changes, bounded streaming
updates, tool intents and results, compaction records, usage, errors, and
archive/restore events. Preserve source labels and causal request/tool IDs.
Distinguish model-requested actions from core-observed execution results.

The following is an illustrative valid Unix v1 subset. Its UUIDs and timestamp
are examples. `workspace.units` contains the bytes for `/work/project`. The
example is not evidence of release schema compatibility:

```jsonl
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"f23d5e17-ef07-40dd-8db2-6e6b4a0ce12b","seq":0,"timestamp":"2026-11-08T12:34:56.789Z","type":"session.started","payload":{"title":"Project overview","workspace":{"encoding":"unix_bytes","units":[47,119,111,114,107,47,112,114,111,106,101,99,116]},"workspace_id":"8a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","app_version":"0.1.0"}}
{"schema_version":1,"session_id":"7a0d3e5c-8c2b-4b9c-a6de-19f1c8d2e470","event_id":"eac73dca-838e-4802-a0eb-8bafbf00cd7c","seq":1,"timestamp":"2026-11-08T12:34:57.000Z","type":"message.recorded","payload":{"role":"user","source":"user_cli","text":"Explain this project."}}
```

For release, define and version the complete event schemas, limits, and
migration rules. Preserve unsupported data for inspection, but do not resume a
journal with unknown required semantics or schema versions. Invalid sequence
numbers, duplicate IDs/keys, or conflicting session IDs require a visible
recovery/read-only state, not silent reinterpretation. The current prototype
rejects such journals and leaves their bytes unchanged; it has no recovery
artifact, repair, or read-only view. Unsupported records remain in the original
file, but the CLI does not expose them through an inspection view.

Store large outputs and binary attachments as bounded artifacts referenced by
safe session-local identifiers, with content digests and availability status.
Validate references within application-managed storage; never treat a transcript
path or URI as permission to read a host file or contact a service. Missing
artifacts remain visible as missing. Do not invent their content.

## Release durability, recovery, and locking

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

## Release archive, restore, and resume

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

## Release privacy and trust

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

## Release acceptance requirements

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
