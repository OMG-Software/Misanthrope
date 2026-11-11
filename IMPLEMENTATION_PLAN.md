# OMGCode implementation plan

Last verified: 2026-11-11.

This guide is part of `main` after PR #9. Read it before selecting the next
increment. Checkpoint preparation does not mean native replacement exists.

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
- **PR #10** merged at `176637f`: the accepted replacement/checkpoint/recovery
  contract in
  [ADR-0018](docs/adr/0018-define-checkpointed-replacement-before-enabling-overwrites.md)
  and the [file-access guide](docs/file-access.md#replacement-contract).
  The merge was verified through GitHub on 2026-11-09. It changed documentation
  only.
- **PR #11** merged at `4da432f`: Phase A2a read-only core inspection of caller-supplied
  replacement records. See [ADR-0019](docs/adr/0019-validate-replacement-records-before-native-storage.md).
- **PR #12** merged at `9e616a3`: the OMGCode rename, version 0.7.0.
- **PR #13** merged at `22040d5`: **Phase A2b**, version **0.8.0**, with
  protected Linux checkpoint storage and existing-only CLI recovery.
- **PR #14** merged at `b3829db`: typed checkpoint inspection state and
  A2b review fixes, with no JSON or storage format change.
- **PR #15** merged at `4fe92ca`: **Phase A2c**, version **0.9.0**, with trusted
  Linux ext4 apply. Its pull-request checks passed, but the Rust workflow failed
  on the merge commit because of the test race described in the known cautions.
- **PR #16** merged at `b7fbc0b`: fixes that test race, with tests and
  documentation only.
- **PR #17** merged at `ddfc90a`: **Phase A3a**, the Windows NTFS private store.
- This branch, `codex/windows-apply`: **Phase A3b**, version **0.10.0**, with
  Windows NTFS checkpoint storage and trusted apply.

Implemented today:

- Pinned Rust 1.99.1 workspace with `omgcode-core`, `omgcode-cli`, and
  `omgcode-platform`.
- Compiled, versioned base prompt `0.1.4` with a visible digest.
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
- Read-only core initialization/ledger record inspectors with bounded strict
  JSON, lifecycle validation, manifest references, and unresolved-state retention.
  The pure inspectors open no files and execute no recorded work.
- Linux protected checkpoint preparation through the core broker, strict
  manifests and payload checks, bounded initialization, quota and recovery
  capacity, and fail-closed admission. No workspace entry is replaced.
- Existing-only `file checkpoints` and `file acknowledge-checkpoint` recovery.
  Inspection returns metadata. Acknowledgement clears a blocker without success,
  repair, permission changes, or replay.
- Trusted `file apply` on Linux ext4: exact precondition, stopped-writers
  acknowledgement, private stage, durable commit intent, one native rename,
  and verified sync/outcome ordering. See [ADR-0022](docs/adr/0022-publish-one-verified-stage-on-linux-ext4.md).
- Windows NTFS checkpoint storage and trusted `file apply`, with no power-loss
  claim. See [ADR-0024](docs/adr/0024-publish-one-verified-stage-on-windows-ntfs.md).

Not implemented:

- Apply on other filesystems, restore, deletion, and standalone CLI checkpoint
  preparation.
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

2. Verify that the preceding phase's PR is merged. A2b and its review fixes
   are merged as PRs #13 and #14. Verify the A2c PR before starting another
   increment. Preserve its feature branch while it is under review.
   Do not merge a PR without the user's authorization.
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
checks that actually ran. Application tests were not applicable to A1; A2a
requires the Cargo checks and targeted `replacement_records` integration tests.
A2b adds `checkpoint_storage`, `checkpoint_recovery`, native private-store and
capture tests, and internal persistence fault/reopen tests. Run the complete
suite on both hosts with default parallelism, as CI does. Use
`-- --test-threads=1` only as an additional run when host contention affects local
provider fixtures; record failures before any isolated rerun or full retry. A
serial run alone cannot detect races between test threads.

A2b local verification on 2026-11-10 passed formatting, Clippy, serial all-target
tests, and doc tests with Rust 1.99.1. Ubuntu 24.04 under WSL, uid 1000, passed
266 tests and one doc test. Native Windows passed 235 tests and one doc test.
Each command exited zero; no tests failed, were ignored, or were filtered.
The final provider fixtures did not time out. Earlier baseline timeout evidence
is retained; no assertion or timeout was weakened. These local results do not
establish remote CI status or native replacement qualification.

A2c started from the verified `b3829db` merge. The unchanged baseline passed
266 Linux and 235 Windows tests, plus one doc test on each host. Windows needed
workspace-local `TEMP` and `TMP` and an execution context that permits native
hard links and archival operations. Keep the original access-denied results;
do not weaken assertions to make a restricted run pass.

A2c final verification passed formatting, Clippy, serial all-target tests,
and doc tests with Rust 1.99.1. Linux ran 293 tests and one doc test. Native
Windows ran 237 tests and one doc test. Each final command exited zero; no tests
failed, were ignored, or were filtered. Linux apply ran on ext4 under WSL
with a separate tmpfs mount for the cross-mount refusal case. See the
[native apply limits](docs/file-access.md#implemented-native-apply-phase-a2c).

The lock-race fix started from `4fe92ca`, where Linux `checkpoint_recovery` failed
7 of 150 parallel runs and `cli_workflow` failed 7 of 150. After the fix, each of
those suites and `replacement_apply` had no failures in 300 parallel runs. Final
verification used default test parallelism with Rust 1.99.1 and passed
formatting, Clippy, all-target tests, and doc tests. Linux under WSL ran 294 tests
and one doc test. Native Windows ran 238 tests and one doc test, with
workspace-local `TEMP` and `TMP`. No tests failed, were ignored, or were filtered.

A3b final verification used default test parallelism with Rust 1.99.1 and passed
formatting, Clippy, all-target tests, and doc tests. Native Windows ran 309 tests
and one doc test, with `TEMP` and `TMP` on the same NTFS volume as the fixtures.
Linux under WSL ran 296 tests and one doc test. No tests failed, were ignored, or
were filtered. Temporary mutations of the Windows refusal checks were each caught
by a test, except for two that do not change behaviour on the tested host: a link
check that `stage_metadata` repeats, and the first of two identity comparisons in
the recheck. These results do not establish remote CI status, power-loss behaviour,
or release filesystem support.

A mock or compile-only result does not establish native containment, live
provider compatibility, or release filesystem support.

## Phase plan

### Phase A: replacement contract, checkpoints, and recovery

The read-only preview is evidence, not an apply operation. Replacement needs a
separate reviewed contract before any overwrite is enabled. Split this phase
at that review boundary:

- **A1 (merged as PR #10):** accepted contract and ADR, platform gates, and
  acceptance cases. It completed contract review, not Phase A.
- **A2:** implement and verify the accepted contract in bounded increments.
  Review platform wrappers and storage compatibility before enabling a trusted
  CLI apply command. An unqualified backend must refuse replacement. Do not
  treat passing existing read/create tests as replacement evidence. The first
  recovery surface is inspection and acknowledgement. Explicit restore remains
  a later feature with its own current-content and checkpoint tests.

  - **A2a (merged as PR #11):** read-only byte-input initialization/ledger validation.
    Preserve recorded uncertainty and test malformed, incomplete, and bounded
    histories. No native storage, writer, CLI, or admission change.
  - **A2b (merged as PR #13):** protected Linux native store and initialization,
    manifest/payload validation, aggregate quota, reserved-capacity durable
    append, existing-only inspection, and fault/reopen tests. Integrate live
    admission with native storage and existing-only missing-state rules.
    Library-only preparation stops unresolved; CLI inspection/acknowledgement
    does not apply an edit. See [ADR-0021](docs/adr/0021-store-checkpoints-with-native-private-handles.md).
  - **A2c (merged as PR #15):** trusted Linux ext4 apply with exact supported
    metadata, durable intent, native publication, and uncertain-outcome recovery.
    Other filesystems and Windows refuse apply. Release filesystem and power-loss
    qualification, Windows ACLs, and restore remain open.
  - **A3a (merged as PR #17):** Windows NTFS private store with exact per-user
    DACLs, as a separate public type the core did not yet use. See
    [ADR-0023](docs/adr/0023-store-windows-checkpoints-with-verified-acls.md).
  - **A3b (this branch, `codex/windows-apply`):** Windows capture,
    security-metadata preservation, and retained-handle publication, with the
    store wired to `PrivateDirectory` and the core's refusal lifted. See
    [ADR-0024](docs/adr/0024-publish-one-verified-stage-on-windows-ntfs.md).
    Release qualification, power-loss evidence, and restore remain open.

Contract decisions accepted in A1:

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

The [accepted replacement contract](docs/file-access.md#replacement-contract)
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

- Linux CLI integration tests raced on inherited `flock` descriptors, producing
  `Busy` and mock-provider timeouts. The spawn gate in
  [testing standards](docs/testing-standards.md#isolation-and-concurrency) covers
  the CLI suites. `omgcode-core`'s `file_broker` FIFO test still spawns one child
  beside in-process stores; it did not fail in 60 parallel runs and is not gated.
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
  and release filesystem support remain unqualified. The Windows directory flush
  has no Microsoft durability contract; never describe it as power-loss safe.
