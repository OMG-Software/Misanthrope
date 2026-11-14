# 0039: Gate future model tools with bound, single-use approval records

- Status: Accepted
- Date: 2026-11-14

## Context

Model-authorised tools need per-call approval. Prompt text, a model claim, or a
journal record cannot authorize a host action. The current chat is one
non-interactive turn and there is no approval UI. A future tool therefore
needs a trusted, durable decision that binds the exact request and cannot be
reused.

The [security rules](../security-coding-rules.md) require binding
authorization to a session, workspace, operation, target, policy revision, and
lifetime, and invalidating pending grants when policy changes.

## Decision

Record approvals in the bounded control journal and expose trusted CLI
commands:

```text
omgcode approvals request --session <uuid> --workspace <id>
    --operation <command|file_write|git_push|network>
    --digest <sha256> --summary <text> [--ttl-secs <60-3600>]
omgcode approvals list
omgcode approvals show <id>
omgcode approvals approve <id>
omgcode approvals deny <id>
omgcode approvals consume <id> --session <uuid> --workspace <id>
    --operation <operation> --digest <sha256>
```

Rules:

- **Binding:** a request records its session, workspace, operation, exact
  request SHA-256, settings revision, creation time, and expiry. It stores no
  argument text. The summary is limited to 256 bytes, must be non-empty, and
  must contain no NUL or control characters; it must never contain secrets.
- **Lifetime:** 60 to 3600 seconds. An expired pending or approved request
  cannot be decided or consumed.
- **Single use:** an approved request is consumed once. Consumption checks the
  session, workspace, operation, digest, and that the settings revision is
  still the one recorded at request time. Any settings change invalidates the
  approval.
- **Revision:** approval events preserve the settings revision. A request
  requires an existing registered workspace; the CLI request command is a
  trusted operator action.
- **No execution:** the CLI consume command only marks the record used. No
  model tool executes in this phase. A future tool must call the core
  consumption check immediately before its operation and must fail closed if
  the approval is missing, denied, expired, consumed, or bound differently.
- **Limits:** at most 256 retained approval records and 64 undecided ones.
  Retention is not compaction; a full journal refuses new requests.

## Alternatives considered

- **Prompt-only consent or a model claim of consent.** Rejected: prompt text
  grants nothing.
- **A session-wide or workspace-wide allowlist.** Rejected: it authorizes more
  than one call and cannot bind the exact request.
- **A separate approvals journal.** Deferred: the control journal already
  provides durable single-writer authorization state with a settings
  revision, and approvals preserve that revision.
- **Automatic approval after a timeout.** Rejected: silence is not consent.

## Consequences

- ADR-0040 later added the command tool that consumes an approval. Until then,
  no model tool, UI, or execution path existed.
- The operator uses CLI commands until an approval UI exists. The CLI request
  command is a testing and manual-flow surface; a model tool would create the
  request through the same core method.
- A settings change between approval and consumption invalidates the approval,
  even when the change is unrelated. This is conservative and visible.
- Retained approval records consume control-journal capacity. Archiving or
  compaction is a separate concern.
- A journal written by 0.24.0 cannot be read by 0.23.0.

## References

- [Agent instructions](../agent-instructions.md)
- [Workspace settings](../workspace-permissions.md)
- [Security coding rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md)
- [ADR-0038](0038-record-provider-profiles-without-credentials.md)
