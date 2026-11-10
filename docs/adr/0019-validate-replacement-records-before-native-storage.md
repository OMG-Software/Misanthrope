# 0019: Validate replacement records before native storage

- Status: Accepted
- Date: 2026-11-09

## Context

PR #10 accepted the replacement/checkpoint/recovery contract. It requires a
separate strict ledger and a crash-safe initialization anchor. Reopening must
retain incomplete or uncertain work without replay. The native private-store,
security-metadata, ACL, and persistence gates remain unimplemented. Enabling an
overwrite command while those boundaries are incomplete would violate ADR-0018.

## Decision

Implement Phase A2 in bounded increments. Phase A2a adds read-only core APIs
that inspect caller-supplied record bytes. They have no filesystem I/O, writer,
CLI command, or broker-admission integration. They validate the complete input
before returning evidence and never execute an action. A parsed ready/prepared
claim cannot establish native durability or authority.

Use a common strict version-1 envelope for initialization and ledger records.
Reuse recursive duplicate-key rejection, native path metadata, and broker path
syntax. Match expected store IDs, validate RFC 4122 version-4 UUIDs and UTC
timestamps, and require contiguous sequences and unique event IDs. Keep one
unresolved operation and every historical operation ID. Bound bytes, records,
descriptors, and retained intents before accepting them.

Record a fixed artifact-name plan beneath an operation UUID entry. Bind the
manifest's exact encoded byte count and digest without inventing a native
security-metadata parser in this increment. A later private-store reader must
validate the actual manifest, payloads, native objects, and aggregate quota.
Count every retained intent conservatively, including acknowledged attempts
that may not have created an artifact directory.

Keep progress separate from outcome and acknowledgement. End-of-input preserves
unfinished progress. Acknowledgement clears only the recorded blocker and never
invents success. For this first format, a post-commit non-success result must be
uncertain. A proven backend-specific clean refusal after commit intent requires
a separately reviewed format extension before a writer can record it.

These APIs compile on both targets but require this OS's native target encoding.
They do not convert foreign-OS journals. The
[file-access guide](../file-access.md#implemented-record-inspection-phase-a2a)
owns exact fields, limits, and remaining native gates.

## Alternatives considered

- Add the parser and native writer together: deferred. Reviewing deterministic
  state reconstruction separately makes the incomplete-history rules testable
  before persistent files or workspace writes exist.
- Parse into ordinary JSON values first: rejected because duplicate keys could
  be discarded before strict typed decoding sees them.
- Reuse file-operation-v1 variants: rejected because old strict readers would
  reject new events. Existing settings, operation, and session formats stay
  unchanged.
- Treat parsed readiness or completion as an access grant: rejected because
  supplied records are untrusted history and cannot validate current objects.
- Silently translate a foreign native path encoding: rejected because lossy
  conversion cannot preserve an access target. Cross-OS conversion is outside
  this increment.
- Allow a generic clean failure after commit intent: rejected because the parser
  cannot establish a no-mutation native error. Retain uncertainty until a
  backend-specific contract exists.

## Consequences

The core has a bounded, tested record-validation API. The application version
becomes 0.6.0; dependency versions and features do not change. The current CLI,
file broker, permissions, and stored formats are unchanged. No production path
creates these new journals yet.

Phase A2b must implement protected native storage, initialization, checkpoint
manifests/payloads, quota, durable append, and existing-only inspection before
changing broker admission. Phase A2c must qualify native replacement and add
trusted CLI apply/acknowledgement. Restore remains a later feature. Passing A2a
tests does not complete Phase A or qualify either replacement backend.

Phase A2b is implemented by
[ADR-0021](0021-store-checkpoints-with-native-private-handles.md). It adds Linux
storage and existing-only inspection/acknowledgement. Native publication and
trusted CLI apply remain A2c gates. The historical A2a scope above is unchanged.

## References

- [Implementation plan](../../IMPLEMENTATION_PLAN.md)
- [Replacement contract](../file-access.md#replacement-contract)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [Security rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md#replacement-acceptance-gates-planned)
