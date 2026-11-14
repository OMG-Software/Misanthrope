# OMGCode implementation plan

Last verified: 2026-11-14.

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
- **PR #18** merged at `243ba8d`: **Phase A3b**, version **0.10.0**, with
  Windows NTFS checkpoint storage and trusted apply.
- **PR #19** merged at `270b889`: **Phase A4**, version **0.11.0**, with
  explicit restore and checkpoint deletion.
- **PR #20** merged on 2026-11-11: **Phase B**, version **0.12.0**, with
  read/list model tools in chat behind the `model_tools` workspace preference.
- **PR #21** merged at `6b1ebe4`: **Phase C1**, version **0.13.0**, with the
  execution contract and an unexposed process-tree runner core.
- **PR #23** merged at `2d44d41`: **Phase C2**, version **0.14.0**, with
  `run_confined`: Landlock and a seccomp socket filter on Linux, and a per-run
  Unix-socket mode.
- **PR #24** merged at `aa31976`: **Phase C3**, version **0.15.0**, with
  Windows AppContainer confinement and recorded, revocable container grants.
- **PR #25 and PR #26** merged at `4eeb12f`: **Phase C4**, version
  **0.16.0**, with the trusted CLI `run`, toolchain profiles, the run journal,
  and the `host_services` preference.
- **PR #27** merged at `ac01a74`: **Phase C5**, version **0.17.0**, with
  tree-wide memory, process, and CPU-time limits.
- **PR #29** merged at `4c2a561`: **Phase C6**, version **0.18.0**, with
  outbound network access behind `task_network` and `--network`.
- `codex/disk-limit` (local commit `0426e42`): **Phase C7**, version
  **0.19.0**, with a sampled net disk-growth limit for every trusted run.
- This branch, `codex/package-network-profile`: **Phase C8** at `16d8945`,
  the contract-only package-network profile and implementation gate
  (ADR-0034), **Phase C9**, version **0.20.0**, with operator-declared
  package-network profile records, **Phase C10**, version **0.21.0**, with
  the trusted allowlisted package fetch and private cache (ADR-0035), and
  **Phase C11**, version **0.22.0**, with read-only Linux run access to that
  cache (ADR-0036), **Phase C12**, version **0.23.0**, with per-workspace
  cache roots and scoped Windows access (ADR-0037), **Phase C13**, version
  **0.24.0**, with provider-profile records and trusted per-call approvals
  (ADR-0038, ADR-0039), and **Phase C14**, version **0.25.0**, with the
  approval-gated model command tool (ADR-0040), qualified by **Phase C15**, a
  native end-to-end acceptance test of one approved successful command on both
  hosts. It is stacked on C7 at
  `0426e42`. No run accepts a profile or has destination-scoped networking;
  no provider adapter is enabled, and the command tool is the only model tool
  that can request a process.

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
  policy bindings. Chat dispatches it through the LM Studio adapter for a
  workspace that enables `model_tools`.
- Read-only `file diff`: trusted CLI preview of a broker-read file against
  complete proposed UTF-8 content. Exact digests, bounded one-hunk diff, no
  workspace mutation, no checkpoint, no replacement.
- LM Studio Chat Completions chat with non-streaming requests. It is text-only
  unless the session's workspace enables `model_tools`; then it offers the read
  and list tools and, when `execute`, `read`, and `write` also allow it, the
  approval-gated command tool. See
  [ADR-0026](docs/adr/0026-offer-read-list-tools-through-a-workspace-preference.md)
  and [ADR-0040](docs/adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).
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
- `process::run_confined` on Linux: a Landlock allowlist (ABI 3 or later),
  seccomp refusal of IP sockets and `io_uring`, and `PairsOnly` (default) or
  `HostServices` Unix-socket access. See
  [ADR-0028](docs/adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).
- `process::run_confined` on Windows: a per-workspace AppContainer with no
  capabilities, checked against persistent ACL grants, with `HostServices`
  refused. Trusted `workspace container-grant` and `container-revoke` record
  each intent and outcome in the control journal. See
  [ADR-0029](docs/adr/0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md).
- Trusted `omgcode run` on Linux and Windows:
  - behind the live `execute` preference, through `run_confined` only;
  - operator-declared toolchain profiles (`workspace profile-add`,
    `profile-show`, `profile-remove`) and a curated environment;
  - a bounded `runs-v1.jsonl` journal of intents and outcomes, with no
    argument text or output, managed by `runs audit`, `acknowledge`, and
    `archive`;
  - a `host_services` preference that a run's `--host-services` flag also
    needs (Linux only);
  - `--network` behind the live `task_network` preference;
  - memory, process, CPU-time, and sampled net disk-growth limits, recorded in
    the run journal;
  - interrupt-to-cancel handling.

  See [ADR-0030](docs/adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md),
  [ADR-0031](docs/adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md),
  [ADR-0032](docs/adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md),
  and [ADR-0033](docs/adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
- A [package-network profile](docs/execution.md#package-network-profile-contract-only)
  contract and implementation gate (ADR-0034). Version 0.20.0 records
  operator-declared profiles as configuration only. No run accepts a profile,
  no destination filter exists, and `--network` remains all-or-nothing.
- A trusted [`packages fetch`](docs/execution.md#trusted-package-fetch)
  command (ADR-0035). It contacts only declared origins, refuses redirects and
  credentials, bounds and hashes the response, writes a private per-workspace
  cache, and records the intent and outcome. The run remains offline and does
  not yet read the cache.
- Read-only run access to a per-workspace fetched-package cache root
  (ADR-0037, superseding ADR-0036). The root is selected with
  `workspace set-package-cache`, exposed read-only on Linux and Windows, and
  named by `OMGCODE_PACKAGE_CACHE`. Windows requires a read-execute container
  grant on the root; Linux falls back to application storage when no root is
  selected. Overlapping roots are refused.
- Provider profiles record connection shapes without credentials or adapters
  (ADR-0038). Trusted approval records bind a future model-tool call to a
  session, workspace, operation, request digest, and settings revision, expire,
  and are consumed once (ADR-0039). Provider profiles enable no connection; the
  command tool is the only model tool that consumes an approval record.
- The `workspace_run_command` model tool (ADR-0040) is offered only with live
  `model_tools`, `execute`, `read`, and `write`. Its first call records a
  pending approval and starts nothing; an approved exact call consumes the
  approval and uses the trusted runner with fixed limits. No write, network,
  Git, or MCP model tool exists.
- Trusted `file restore` through a new checkpoint and
  `file delete-checkpoint --permanent` for one resolved checkpoint, on the same
  filesystems as apply. Neither is power-loss qualified. See
  [ADR-0025](docs/adr/0025-restore-and-delete-checkpoints-explicitly.md).

Not implemented:

- Apply or restore on other filesystems, workspace file deletion, ledger
  compaction, and standalone CLI checkpoint preparation.
- Model write, network, Git, and MCP tools; live provider tool-call
  qualification; and streaming, discovery, and cancellation in a live provider.
- A run network destination allowlist, runtime skills, memories, desktop/UI
  panels, compaction, and caches. Nothing calls the unconfined process runner.
- Provider adapters and subscription/API-key login; the recorded provider
  profiles are configuration only.

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

A4 started from the unmerged PR #18 head `2dc3b50`. The unchanged baseline passed
309 Windows and 296 Linux tests, plus one doc test on each host. The first
Windows run failed 10 platform tests with access denied when a fixture opened
with `WRITE_OWNER`. The worktree's `TEMP` inherited only modify access for
Authenticated Users under an unelevated token. Pointing `TEMP` and `TMP` at a
workspace-local directory that grants the current user full control fixed it.
No assertion was weakened. A4 final verification ran after merging PR #18's head
`17c48ec`, with Rust 1.99.1 and default test parallelism. It passed formatting,
Clippy, all-target tests, and doc tests. Native Windows NTFS ran 324 tests and
one doc test. Linux ext4 under WSL, uid 1000, ran 310 tests and one doc test. No
tests failed, were ignored, or were filtered. These results do not establish
remote CI status, power-loss behaviour, or release filesystem support.

Phase B started from `main` at `270b889`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows NTFS ran 339 tests and one doc test. Linux
ext4 under WSL, uid 1000, ran 325 tests and one doc test. No tests failed, were
ignored, or were filtered. Temporary removal of path confinement, the core
tool-text filter, and the CLI filter call were each caught by a test. These
results use a mock LM Studio server. They do not establish live tool-call
compatibility, remote CI status, or release readiness.

A review follow-up then withheld saved tool results without live read
authority, refused token-bearing tool calls, reported the final finish reason,
and gated tool mode by platform. The same checks passed: Windows NTFS ran 345
tests and one doc test, and Linux ext4 under WSL ran 331 tests and one doc test,
with none failed, ignored, or filtered. Forcing replay in the runtime or text
chat, removing the token refusal or its nested-argument decoding, and dropping
the finish reason in the runtime or provider were each caught by a test.

An opt-in live spot-check then ran the debug CLI on Windows against LM Studio 0.4.28 and `google/gemma-4-e4b`
at `http://127.0.0.1:1234/v1`, with no token and disposable data. Text-only chat
with the preference off, a read, and a directory listing succeeded. Revoking
read produced `tool_denied`. Explicit parent, absolute, rooted, and `..` listing
calls each failed as `invalid_tool_call` before file access. Sentinels outside
the workspace never reached the journal. This is one model on one host, not a
provider qualification.

C1 started from PR #20's head `14cf21f`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows ran 353 tests and one doc test. Linux ext4
under WSL (kernel 6.6, uid 1000) ran 338 tests and one doc test. No tests failed,
were ignored, or were filtered. Five temporary mutations were each caught:
removing the Linux seccomp filter, close-on-exec marking, or kill after a normal
exit, and allowing Windows job breakaway or dropping the handle list. These
results do not establish confinement, remote CI status, or release readiness.
After PR #20's review follow-up was merged in, the same checks passed with 359
Windows tests and 344 Linux tests, each with one doc test, and none failed,
ignored, or filtered.

C2 started from `main` at `6b1ebe4`. Its final verification ran with Rust
1.99.1 and default test parallelism. It passed formatting, Clippy, all-target
tests, and doc tests. Native Windows ran 361 tests and one doc test. Linux ext4
under WSL (kernel 6.6, Landlock ABI 3, uid 1000) ran 364 tests and one doc
test. No tests failed, were ignored, or were filtered. Eight temporary
mutations were each caught: removing `landlock_restrict_self`, allowing
`socket` in `PairsOnly`, allowing `io_uring_setup`, leaving truncation
unhandled, following symbolic links in rule paths, allowing datagram pairs,
allowing non-Unix families in `HostServices`, and accepting special files as
rule paths. These results do not establish confinement on other kernels,
remote CI status, or release readiness.

PR #23's first CI run failed one Windows test, the C1 escape test, which had
also failed on `main` at `6b1ebe4`. Windows reported the job empty before the
killed grandchild's process object was signaled. It did not fail in 200 local
runs or 40 local suite runs. The test now waits up to five seconds for that
object; a surviving grandchild still fails it. Review fixes then qualified the
allowlist claims: Landlock does not restrict metadata changes, and does not
restrict device `ioctl` before ABI 5. Re-verification passed formatting,
Clippy, all-target tests, and doc tests: Windows 362 tests and Linux 365, each
with one doc test, none failed, ignored, or filtered.

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
  recovery surface is inspection and acknowledgement. Explicit restore was
  left to A4, with its own current-content and checkpoint tests.

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
    Release qualification and power-loss evidence remain open.
  - **A4 (this branch, `codex/checkpoint-restore-delete`, stacked on PR #18):**
    explicit trusted restore through the apply path and a new checkpoint, and
    permanent deletion of one resolved checkpoint. Deletion uses intent and
    outcome ledger records and non-recursive single-entry native removal. It
    keeps history and frees a retained slot. Inspection and acknowledgement
    stay read-only. See
    [ADR-0025](docs/adr/0025-restore-and-delete-checkpoints-explicitly.md).
    Ledger compaction, release qualification, and power-loss evidence remain
    open.

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

**PR #20, `codex/model-read-tools`.** The user chose a persisted
workspace preference with no per-call prompt, and required that a model call
never read outside the registered workspace. Chat offers the read and list tools
only when the session's workspace enables `model_tools`. The dispatcher rechecks
that preference live before each call. Model paths are refused before the
broker when absolute, rooted, prefixed, or parent-relative; the broker still
refuses links and names outside the workspace. The LM Studio adapter parses tool
calls strictly, and the configured token is removed from tool results before
they are recorded or sent. Earlier tool results are replayed only under live
`model_tools` and `read` preferences, and token-bearing calls are refused. See
[ADR-0026](docs/adr/0026-offer-read-list-tools-through-a-workspace-preference.md).
Live LM Studio tool-call compatibility remains open.

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

Follow [docs/execution.md](docs/execution.md),
[docs/development-tools.md](docs/development-tools.md), and
[docs/platform-support.md](docs/platform-support.md). Add confined execution
only with explicit toolchain profiles, bounded output, controlled networking,
and observed process-tree cleanup. Never fall back to unrestricted execution.

The user chose Landlock and seccomp on Linux, AppContainer inside a Job Object
on Windows, and trusted CLI commands as the first caller. See
[ADR-0027](docs/adr/0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md).

- **C1 (PR #21):** the
  execution contract and a process-tree runner core in `omgcode-platform`.
  It covers bounded output, deadline, cancellation, and observed cleanup. It
  denies Linux process-group escape with seccomp and Windows job breakaway.
  There is no confinement and no caller.
- **C2 (PR #23):** Linux Landlock filesystem
  rules and seccomp network denial. The user chose a per-run Unix-socket mode:
  `PairsOnly` by default, and `HostServices` for developers who need the
  Docker daemon or similar services. See
  [ADR-0028](docs/adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).
- **C3 (PR #24):** Windows AppContainer with
  recorded, reversible workspace access entries and no network capability. The
  user chose persistent grants with explicit revocation, read-execute grants
  for toolchain roots, refusing `HostServices` on Windows for now, and journal
  records with trusted CLI commands. See
  [ADR-0029](docs/adr/0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md).
- **C4 (PR #25 and PR #26):** trusted CLI `run` behind the `execute`
  preference. The user chose:
  - operator-declared toolchain profiles recorded with a revision;
  - a separate bounded run journal that stores no output;
  - a `host_services` preference plus a per-run flag;
  - acceptance workflows for Rust, Python, Node, C/C++, Go, and Java on both
    platforms.

  Java has not run, because no JDK was installed. See
  [ADR-0030](docs/adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md).
- **C5 (merged as PR #27):** tree-wide limits for every trusted run. The user
  chose:
  - a delegated cgroup v2 on Linux, with OMGCode started in its own
    `systemd-run --user --scope -p Delegate=yes` scope;
  - limits always applied, failing closed where they cannot be;
  - memory, process-count, and CPU-time limits;
  - `run` flags with defaults, recorded in the run journal.

  See
  [ADR-0031](docs/adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md).
- **C6 (merged as PR #29):** outbound network access behind `task_network` and
  `--network`, with resolver and public certificate paths on Linux. See
  [ADR-0032](docs/adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md).
- **C7 (local commit `0426e42` on `codex/disk-limit`):** a sampled net
  disk-growth limit for every trusted run. The user chose a disk limit as the
  next increment. The supervisor measures the writable roots before the run
  and once a second on a separate thread, fails closed when it cannot measure,
  and records the limit and peak growth. See
  [ADR-0033](docs/adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
- **C8 (this branch, `codex/package-network-profile`):** the package-network
  profile contract and implementation gate. The user chose a package-network
  profile, then chose contract-only after the primitive review. No run accepts
  a profile; an advisory proxy is rejected because the tree can bypass it.
  See [ADR-0034](docs/adr/0034-gate-package-networking-behind-enforceable-destination-control.md).
- **C9 (this branch, version 0.20.0):** operator-declared package-network
  profile records. Profiles are stored through the control journal, with
  revisions and strict origin validation. They are configuration only; no run
  accepts one and no destination filter exists.
- **C10 (this branch, version 0.21.0):** the trusted allowlisted package
  fetch. The user selected a trusted host-side fetch into a private cache. The
  command contacts only declared origins, refuses redirects and credentials,
  bounds and hashes the body, and audits the intent and outcome. The run
  remains offline and does not read the cache. See
  [ADR-0035](docs/adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md).
- **C11 (this branch, version 0.22.0):** read-only Linux run access to the
  workspace package cache. Superseded by C12, which moves exposure to a
  per-workspace root and adds Windows access.
- **C12 (this branch, version 0.23.0):** per-workspace cache roots and a
  scoped Windows grant. The operator selects a root outside application
  storage and the development root; fetch and run use it; Windows needs a
  read-execute grant on that root. See
  [ADR-0037](docs/adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md).
- **C13 (this branch, version 0.24.0):** provider-profile records and trusted
  per-call approval records. Profiles store only a name, kind, and
  authentication mode; no credential, endpoint, adapter, or login exists.
  Approvals are bound, expiring, single-use records; the first consumer is the
  command tool in C14. See
  [ADR-0038](docs/adr/0038-record-provider-profiles-without-credentials.md) and
  [ADR-0039](docs/adr/0039-gate-future-model-tools-with-bound-single-use-approval-records.md).
- **C14 (this branch, version 0.25.0):** the `workspace_run_command` model
  tool. It is offered only with live model-tools/execute/read/write, records a
  pending approval on the first call, and consumes an approved exact record
  before calling the trusted runner. It uses fixed limits and no network or
  host services. See
  [ADR-0040](docs/adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).
- **C15 (this branch, version 0.25.0 qualification):** native end-to-end
  acceptance for the model command tool. The real chat CLI, a mock provider,
  two turns, an operator approval, and the trusted runner run one successful
  confined command on Linux and Windows. Linux needs a delegated cgroup;
  Windows needs a workspace container grant. The case fails, rather than
  skips, when the host cannot provide them. Confinement correctness remains
  the platform suites' responsibility.

Next decisions, not yet scheduled:

- a credential service, provider adapters, and subscription/API-key login;
- model write, network, Git, and MCP tools;
- registry credentials and development-server networking.

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
- Windows runs under host load can outlast the default 10-second CLI test
  deadline. The disk-limit case now gives the run an explicit 45-second
  deadline and the CLI 60 seconds, so a missed sampler ends as a recorded
  deadline instead of a killed CLI with a pending run. The pre-existing
  multi-fixture provider tests also timed out locally under host load; they
  pass individually and passed CI on `987a5aa`. Recheck timeouts on a clean
  host rather than weakening an assertion.
- `.serena/` is untracked and unrelated. Do not commit or delete it without
  deciding what it is.
- The turn runtime is library/test infrastructure until a provider adapter and
  an explicit tool-dispatch decision exist.
- The edit preview is a snapshot, not a lock or an apply token. A later
  replacement must recheck content identity.
- Native containment, Windows ACL hardening, directory power-loss durability,
  and release filesystem support remain unqualified. The Windows directory flush
  has no Microsoft durability contract; never describe it as power-loss safe.
