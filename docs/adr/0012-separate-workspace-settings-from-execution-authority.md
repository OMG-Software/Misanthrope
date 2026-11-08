# 0012: Separate workspace settings from execution authority

- Status: Accepted
- Date: 2026-11-08

## Context

The next increment needs persistent project identity and user permission choices.
Native file and process controls do not exist yet. A positive settings result
must not provide an accidental route to unrestricted host execution. A path or
historical session ID alone cannot establish the current project identity.

## Decision

Implement trusted CLI settings and configuration-only policy checks before tool
dispatch. Always return `execution_allowed: false`. Keep short-lived bindings
process-local and non-deserializable. Bind them to store, session, workspace,
revision, and monotonic expiry. Use a conservative global revision so every
configuration change invalidates all existing bindings.

Store workspace UUIDs with native directory identity and creation time. Recheck
identity and root membership before binding or evaluating. These are metadata
checks; actual file operations still require object-relative authorization.
Require application storage to be separate from the development root.

Keep settings deltas and audit events in one bounded, locked JSONL journal. Sync
before acknowledgement. Reject malformed existing logs without reset. Audit
failure prevents a policy result from being returned. Preserve legacy text-only
sessions without inferring permissions from their IDs or migrating them silently.

The [workspace settings guide](../workspace-permissions.md) owns exact contracts,
limits, command behavior, and remaining implementation gates.

## Alternatives considered

- Dispatch tools directly from enabled flags: rejected because settings cannot
  contain a process or resolve races on the file actually opened.
- Use paths alone as project IDs: rejected because aliases and replacement can
  transfer preferences to a different directory. Native identity also has limits
  and must not become a substitute for file authorization.
- Store a settings snapshot and append a separate audit file: rejected for this
  increment because a partial update can separate authority from its audit.
- Introduce a database: deferred because this bounded local increment can keep
  one validated source of truth without a new storage dependency. A future
  migration must preserve identity, history, and failure behavior.
- Persist session grants in conversation JSONL: rejected because replayed or
  imported context must never recreate live authority.

## Consequences

The CLI can exercise settings, identity, stale binding rejection, and audit
failure before a tool is exposed. Audit capacity is finite and rotation is not
implemented. Filesystems without native identity or creation-time support fail.
Rebinding replaced directories needs a separate trusted workflow. The next
increment must build the file broker; a confined runner and live revocation
remain later release gates. No new execution route is permitted by this ADR.

## References

- [Security rules](../security-coding-rules.md)
- [Session storage](../session-storage.md)
- [Local chat before execution](0010-build-a-local-chat-core-before-execution.md)
- [Windows FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)
- [GetFileInformationByHandleEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex)
- [Rust directory creation metadata](https://doc.rust-lang.org/std/fs/struct.Metadata.html#method.created)
