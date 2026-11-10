# 0021: Store checkpoints with native private handles

- Status: Accepted
- Date: 2026-11-10

## Context

ADR-0018 requires private, bounded checkpoints before any workspace replacement.
ADR-0019 supplies strict record validation but cannot establish the identity,
privacy, contents, or persistence of native files. Reusing ordinary journal
helpers would not supply that boundary. Preparation must be testable without
enabling an overwrite or treating recorded evidence as authority.

## Decision

Implement Phase A2b as library-only checkpoint preparation on Linux. Keep
replacement capability disabled. Use retained private directory handles,
exclusive creation, native identity and creation-time checks, exact owner/mode
checks, single-link regular files, bounded enumeration, and no-follow child
resolution. Reopen generated procfs references only after pinning and checking
the object. Refuse unavailable controls without a weaker fallback. Windows
refuses checkpoint storage before mutation until a private ACL and directory
persistence backend exists.

Require a fresh binding and both read and write preferences. Capture source
bytes and supported metadata through the same pinned file. Reject source
extended attributes and special mode bits for preparation. Compare the exact
expected digest and size before initializing checkpoint storage. Recheck live
authority before initialization and before per-operation writes. Hold the
control writer throughout the bounded operation. Close the legacy operation
writer before taking the replacement writer; retain the initialization anchor
lock until dependent storage handles close.

Keep separate schema-1 initialization and replacement journals. Existing
settings, session, and file-operation schemas do not change. Sync the anchor,
ledger, payloads, manifests, and parent entries in the required order. Strictly
validate actual files and exact record bytes on reopen. Reserve worst-case
payload, staging, manifest, and recovery-ledger capacity before artifacts.
Count all retained intents and managed file bytes. Reject unknown layouts and
never evict evidence to admit more work.

Persist an intent before artifacts and stop at `checkpoint_prepared`, with
`applied: false`. Leave preparation unresolved until explicit acknowledgement.
Expose existing-only metadata inspection and acknowledgement through trusted
CLI commands. Do not expose preparation, apply, restore, or deletion through
the CLI. Check replacement admission before extending the legacy operation log.
Unused installations retain ordinary file access without provisioning storage.

A failed write or sync poisons the writer and returns uncertainty. Reopen can
validate complete visible records; it cannot prove that a failed sync persisted.
Partial records are invalid. No automatic retry, repair, reset, truncation, or
cleanup occurs. Acknowledgement clears only the recorded blocker.

## Alternatives considered

- Implement storage and native replacement together: deferred. Storage failure
  and recovery need separate evidence before an overwrite is enabled.
- Trust inherited Windows ACLs or omit directory sync: rejected. Neither meets
  the private persistence contract. Unsupported storage must refuse the operation.
- Reopen child paths through ambient paths: rejected. The checked native object
  must remain the object used for I/O.
- Cache validation or trust manifest hashes without reading payloads: rejected
  for this bounded prototype. Full revalidation costs up to 64 MiB per request
  but detects changed retained evidence without a new cache-validity contract.
- Delete incomplete artifacts or evict acknowledged entries: rejected. These
  actions can destroy recovery evidence and need a separate lifecycle design.
- Add replacement events to existing journals: rejected for compatibility with
  their strict readers. No schema migration is introduced.
- Add unsafe user-ID queries or infer identity from environment variables:
  rejected. Enable the existing Rustix dependency's `process` feature for its
  safe effective-user-ID query. This does not add process execution or change
  dependency versions.

## Consequences

The application version becomes 0.8.0. Linux can prepare and verify private
checkpoints without changing the workspace. Tests cover native objects, actual
quota, interrupted initialization, partial writes, sync errors, and reopen.
Windows can report unused state but cannot use present checkpoint storage.
Mode-0755 legacy data roots still support ordinary operations when unused;
checkpoint preparation requires mode 0700 and does not repair it.

Same-device checks are preparation prerequisites, not proof that a future rename
works across mount aliases. Source capture is not an atomic snapshot. Physical
power-loss behavior, foreign-user and privileged mount attacks, Windows ACLs,
metadata publication, and supported filesystem qualification remain open.
Phase A2c must resolve its native publication gates before trusted CLI apply.
Restore remains later work. Checkpoints do not provide process or workspace
isolation, and complete same-user erasure remains undetectable by this layout.

## References

- [Replacement contract and implemented A2b scope](../file-access.md#implemented-checkpoint-storage-phase-a2b)
- [ADR-0018](0018-define-checkpointed-replacement-before-enabling-overwrites.md)
- [ADR-0019](0019-validate-replacement-records-before-native-storage.md)
- [Implementation plan](../../IMPLEMENTATION_PLAN.md)
- [Platform support](../platform-support.md)
- [Testing standards](../testing-standards.md#replacement-acceptance-gates-planned)
