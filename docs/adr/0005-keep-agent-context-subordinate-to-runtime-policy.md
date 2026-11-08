# 0005: Keep agent context subordinate to runtime policy

- Status: Accepted
- Date: 2026-11-08

## Context

The user requires a visible SYSTEM prompt with appended instructions,
configurable token-based compaction, reduced unnecessary model usage through
caching, and a skills settings page. The stated goals are secure operation,
prompt injection defence, and efficiency. These features transform model context
and can carry malicious instructions or stale claims of authorization.

## Decision

Keep authorization, operation state, skill enablement, and instruction revisions
in core-owned records outside generated model context. Make the versioned base
prompt and user additions visible. Keep their provenance through requests,
compaction, and caching. Apply security before efficiency when they conflict.

Use a portable readable compaction path with a token trigger bounded by the
selected route's safe input budget. A 200k setting means 200,000 tokens, not a
promise of model capacity. Use native compaction only through a verified adapter
capability. Cache immutable validated artifacts and supported prompt prefixes;
do not replay model tool calls or treat cached actions as fresh execution.

Use a validated Agent Skills package profile with user-managed enablement.
Packages cannot grant permissions. Run skill scripts through the same controlled
runner as other development tools. See the owning [instruction](../agent-instructions.md),
[context](../context-management.md), and [skills](../skills.md) contracts.

## Alternatives considered

- A hidden or freely replaced base prompt weakens transparency and makes
  instruction provenance difficult to inspect. Use visible append fields.
- Prompts or generated summaries as an authorization source let untrusted text
  manufacture authority. Keep authority in enforced core state.
- Provider-native compaction alone makes portability and visibility depend on
  route-specific features. Keep a harness-managed path.
- Reusing complete agent responses risks stale state and repeated side effects.
  Limit initial caching to defined artifacts and provider prefix processing.
- Automatically executing imported skill code combines content import with
  host execution. Separate import, enablement, and authorized tool use.

## Consequences

Context manifests, invalidation, safe imports, and session reconciliation add
implementation work. Compaction consumes model usage and can lose useful detail;
the journal and evidence references are needed for recovery. Native opaque state
has a visible inspection limit. Neither prompt text nor Rust establishes prompt
injection immunity. The owning guides define acceptance cases; none has run
against an application yet.

## References

- [Security coding rules](../security-coding-rules.md)
- [Testing standards](../testing-standards.md)
- [Agent Skills specification](https://agentskills.io/specification)
