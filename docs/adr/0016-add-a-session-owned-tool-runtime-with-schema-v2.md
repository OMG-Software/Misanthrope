# 0016: Add a session-owned tool runtime with session schema v2

- Status: Accepted
- Date: 2026-11-09

## Context

The prototype had no core owner for a model request and its tool operations.
The v1 session schema recorded only text turns, so it could not preserve tool
intents, core-observed results, or the fact that a manifest advertised tools.
The file-operation journal recorded broker operations but not their causal
model call, and it could not supply a transcript or manifest record. Adding tool
events to v1 without a schema change would contradict the declared exact v1
subset, while rewriting old journals would destroy historical evidence.

The product still has no real provider tool-call adapter. Any initial runtime
must therefore be testable with a caller-supplied model interface and must not
imply CLI or provider tool dispatch.

## Decision

Add `orangecode_core::turn` as the session-owned runtime for one bounded turn.
It requires an open journal, the trusted application-data directory, and a
workspace ID. It opens the control store and creates a fresh process-local
binding before every tool operation, releases the settings lock before the next
model request, dispatches only the existing read and list broker calls, and
enforces round, call, byte, and cooperative-deadline limits. It validates strict
tool-call shapes and arguments before dispatch and labels every result as
core-observed untrusted workspace data.

New sessions write JSONL schema version 2. Version 2 keeps all v1 events and
adds `tool.requested`, `tool.completed`, and `message.recorded` entries with
role `tool` and source `core_tool`. The request manifest gains optional
`tool_schema_version` and `tools` fields. The reader accepts v1 and v2 journals
but rejects mixed versions and any tool event or tool manifest field in v1.
Version 1 journals remain writable with their text-only subset; the turn runtime
refuses them.

The runtime has no CLI command and no real provider adapter. The LM Studio
adapter stays text-only and continues to reject tool-call responses. Writes,
process execution, Git, MCP, and network tools remain unavailable.

The [session-storage contract](../session-storage.md) owns the exact event
sequence and limits. The [file-access contract](../file-access.md) owns broker
checks and recovery.

## Alternatives considered

- Record tool operations only in the file-operation journal: rejected because
  it cannot preserve causal call IDs, source-labelled model context, or an
  accurate request manifest, and it would make resumed inspection ambiguous.
- Add optional tool fields and events to schema version 1: rejected because the
  v1 subset was declared exact, older readers reject unknown fields and events,
  and silent version drift makes compatibility evidence unreliable.
- Migrate v1 journals in place to v2: rejected because it rewrites historical
  bytes and complicates recovery and audit semantics. Keep v1 readable and
  writable instead.
- Wire the runtime to the existing LM Studio adapter immediately: rejected
  because that adapter is text-only and the provider tool-call contract and
  adapter tests do not exist yet. A mock/injected interface is the bounded step.
- Allow the model interface to return writes or process requests: rejected.
  Only the existing read/list broker is available, and no process containment
  exists.

## Consequences

The core now has a bounded lifecycle that can be tested end to end against the
real policy store and broker without exposing a model tool route. The control
writer is held only for each operation, so revocation can take effect before
the next call. Replay verifies that each tool message matches its recorded size
and digest. Tool intents,
results, and messages are durable evidence, and manifests name the exact tool
set. Version-1-only binaries cannot open new version-2 sessions, so mixed-version
deployments must use a current binary or remain on v1 journals. Version 1
sessions cannot use the runtime. The model interface requires a future adapter
to parse provider JSON strictly and map calls without trusting provider
metadata. Secret redaction, real provider compatibility, streaming, compaction,
and execution containment remain separate work.

## References

- [Session storage](../session-storage.md)
- [Workspace file access](../file-access.md)
- [Workspace permissions](../workspace-permissions.md)
- [Agent instructions](../agent-instructions.md)
- [Testing standards](../testing-standards.md)
- [ADR-0012](0012-separate-workspace-settings-from-execution-authority.md)
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
