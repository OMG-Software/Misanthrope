# OrangeCode implementation plan

Last verified: 2026-11-09.

This guide is part of `main` after PR #9. Read it before selecting the next
increment. Its baseline describes merged behaviour; a proposed contract does
not mean its runtime exists.

This is a resume guide, not a replacement for the owning contracts in
[docs](docs/README.md). Read [AGENTS.md](AGENTS.md),
[docs/coding-rules.md](docs/coding-rules.md),
[docs/security-coding-rules.md](docs/security-coding-rules.md), and
[docs/testing-standards.md](docs/testing-standards.md) before changing code.

## Current baseline

- `main` contains **PR #8** at `56ed9bf`: schema-v2 session tool events and the
  core read/list turn runtime.
- **PR #9** merged at `e4dbeca`: read-only edit previews, review fixes, and this
  resume guide. The merge was verified through GitHub on 2026-11-09.
- **Phase A1** proposes the replacement/checkpoint/recovery contract in
  [ADR-0018](docs/adr/0018-define-checkpointed-replacement-before-enabling-overwrites.md)
  and the [file-access guide](docs/file-access.md#proposed-replacement-contract).
  It changes documentation only. Review and accept that contract before A2;
  replacement, checkpoints, and replacement recovery are still unimplemented.
- The application version is **0.5.0**.

Implemented today:

- Pinned Rust 1.99.1 workspace with `orangecode-core`, `orangecode-cli`, and
  `orangecode-platform`.
- Compiled, versioned base prompt `0.1.3` with a visible digest.
- Workspace settings, native directory identity, process-local bindings, and a
  separate settings/audit journal.
- Bounded native read, list, exclusive file creation, and single-directory
  creation through trusted CLI commands, with a separate file-operation journal
  and explicit recovery.
- JSONL sessions with schema version 2: tool requests/results, tool messages,
  archive/restore/interrupt, and strict replay. Version 1 journals remain
  readable and writable with their text-only subset.
- Core turn/tool runtime for read/list through a caller-supplied model
  interface. It has strict tool-call parsing, round/call/byte/prompt limits,
  cooperative cancellation, source-labelled messages, and fresh per-operation
  policy bindings. No CLI command or real provider adapter dispatches it.
- Read-only `file diff`: trusted CLI preview of a broker-read file against
  complete proposed UTF-8 content. Exact digests, bounded one-hunk diff, no
  workspace mutation, no checkpoint, no replacement.
- Text-only LM Studio Chat Completions chat with one non-streaming request and
  no model tools.

Not implemented:

- Existing-file replacement or deletion, and any apply/checkpoint/recovery flow.
- CLI or real-provider model tool dispatch.
- Process execution, execution containment, Git, MCP, runtime skills, memories,
  desktop/UI panels, compaction, or caches.
- Subscription/API-key provider profiles beyond the local LM Studio route.

## Resume steps

1. Inspect the current branch and local changes. Preserve unfinished feature
   work and unrelated files. When there is no unfinished feature work, update
   the baseline:

   ```text
   git fetch origin --prune
   git status --short --branch
   git switch main
   git pull --ff-only
   git status --porcelain
   ```

2. Verify that PR #9 is merged. For A2, also verify that the A1 contract has
   been reviewed and accepted. Do not infer acceptance from this plan or an
   unmerged branch. Do not merge a PR without the user's authorization.
3. Re-read the owning contracts for the selected phase. Do not infer authority
   from this plan, prompts, journals, or model output.
4. Create a `codex/<phase>` branch from a clean, updated `main`.
5. Run the verification baseline before changing code.
6. Carry one bounded phase through implementation, tests, docs, changelog, and
   an ADR when the decision is lasting. Open a PR; do not merge it yourself.

## Verification baseline

Run from the repository root with the pinned toolchain:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

Run native checks on Windows and Linux for runtime changes. Use a separate
target directory for a Linux run so Windows artifacts cannot mask stale
results. Documentation-only increments need local Markdown link/fragment and
whitespace checks, plus the applicable review and analysis checks in
[docs/repository-checks.md](docs/repository-checks.md). That guide describes CI
services; the repository has no standalone documentation checker. Record the
checks that actually ran. Application tests are not applicable to A1.

A mock or compile-only result does not establish native containment, live
provider compatibility, or release filesystem support.

## Phase plan

### Phase A: replacement contract, checkpoints, and recovery

The read-only preview is evidence, not an apply operation. Replacement needs a
separate reviewed contract before any overwrite is enabled. Split this phase
at that review boundary:

- **A1 (current increment):** propose the contract and ADR, define the platform
  gates and acceptance cases, and update the owning guides. Keep runtime
  behaviour and capabilities unchanged. This increment needs contract review;
  it does not complete Phase A.
- **A2 (next after acceptance):** implement and verify the accepted contract.
  Review platform wrappers and storage compatibility before enabling a trusted
  CLI apply command. An unqualified backend must refuse replacement. Do not
  treat passing existing read/create tests as replacement evidence. The first
  recovery surface is inspection and acknowledgement. Explicit restore remains
  a later feature with its own current-content and checkpoint tests.

Contract decisions proposed in A1:

- The expected-content precondition (digest and size) and what happens when the
  target changes before or during the operation.
- Explicitly do not claim atomic compare-and-swap. A hash check followed by a
  rename is not CAS against an independent editor. Keep the limitation visible
  in an ADR and the file-access guide.
- The checkpoint location, digest, retention, and recovery rules. Checkpoints
  must remain outside the workspace and must not overwrite unrelated data.
- Platform replacement primitives and failure modes on Windows and Linux. No
  weaker fallback is allowed.
- Whether the first apply command is trusted CLI only. It must not become a
  model tool before approval, containment, and policy review exist.

The [proposed replacement contract](docs/file-access.md#proposed-replacement-contract)
owns the selected limits, state transitions, and recovery rules. The native
primitive and durability gates there remain implementation prerequisites.

Runtime acceptance criteria for A2:

- Replacement requires a live binding, write permission, and the expected
  current digest/size. A mismatch produces a controlled conflict with no
  mutation.
- A checkpoint of the prior bytes is durable before replacement, with exact
  size/digest and bounded storage.
- Intent/outcome audit and uncertain-outcome handling are explicit. No
  automatic retry, repair, or deletion after an ambiguous result.
- Tests cover stale digest, concurrent edit, links/reparse points, hard links,
  oversize input, uncertain sync, checkpoint recovery, and denied policy.
- No model tool route, process execution, deletion, or recursive operation is
  added in this phase.
- Update [docs/file-access.md](docs/file-access.md),
  [docs/security-coding-rules.md](docs/security-coding-rules.md),
  [docs/testing-standards.md](docs/testing-standards.md), the changelog, and a
  new ADR.

### Phase B: provider tool-call integration for read/list

Only after an explicit product decision on model tool dispatch. The runtime is
already bounded and policy-checked, but the CLI currently advertises no tools.

Acceptance criteria:

- Tools are off by default. A real adapter maps provider responses into the
  runtime without trusting provider metadata or role labels.
- Strict JSON parsing rejects malformed, unknown, duplicate, and oversized
  calls before dispatch.
- Every operation rechecks the live binding and policy at the broker boundary.
- Source-labelled tool content stays user-level; the base prompt remains the
  only system message.
- No write, process, network, Git, or MCP tool is enabled by this phase.
- Local-provider fixtures cover text-only, malformed, unknown, duplicate, and
  policy-denied calls. Live compatibility remains a separate qualification.

### Phase C: real execution backends and toolchain profiles

Follow [docs/development-tools.md](docs/development-tools.md) and
[docs/platform-support.md](docs/platform-support.md). Add confined execution
only with explicit toolchain profiles, bounded output, controlled networking,
and observed process-tree cleanup. Never fall back to unrestricted execution.

### Phase D: additional providers, streaming, and account lifecycle

Follow [docs/provider-authentication.md](docs/provider-authentication.md).
Keep subscription and API-key profiles separate. Do not silently fall back to
paid API usage. Verify current primary documentation before implementing a
login or entitlement route.

### Phase E: desktop interface and session tree

The Rust core remains the authority. The UI must not receive raw journal paths,
credentials, or unrestricted host capabilities. Follow the session, permission,
context, and security contracts.

### Phase F: runtime skills, MCP, memories, compaction, and caches

Follow [docs/skills.md](docs/skills.md), [docs/mcp.md](docs/mcp.md),
[docs/memories.md](docs/memories.md), and
[docs/context-management.md](docs/context-management.md). Imported content is
untrusted; enablement is a trusted user action; stale derived context must be
invalidated.

### Phase G: Git, platform qualification, packaging, and release

Mediated Git workflows need controlled credentials, hooks/helpers, network
policy, and push approval. Platform qualification, security testing, and
release readiness remain separate gates.

## Known cautions

- Local Windows all-target runs intermittently timed out in pre-existing
  multi-fixture provider tests under host load. The tests pass individually and
  CI passed on `987a5aa`; recheck on a clean host rather than weakening a test.
- `.serena/` is untracked and unrelated. Do not commit or delete it without
  deciding what it is.
- The turn runtime is library/test infrastructure until a provider adapter and
  an explicit tool-dispatch decision exist.
- The edit preview is a snapshot, not a lock or an apply token. A later
  replacement must recheck content identity.
- Native containment, Windows ACL hardening, directory power-loss durability,
  and release filesystem support remain unqualified.
