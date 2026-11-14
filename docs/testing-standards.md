# Testing standards

Use these standards with the [coding rules](coding-rules.md) and
[security rules](security-coding-rules.md).

## Current state

The pinned Cargo workspace has three crates: `omgcode-core`,
`omgcode-cli`, and `omgcode-platform`. It includes deterministic core,
CLI, and platform tests. The core includes configuration-only workspace records,
permission preferences, and a local settings/audit journal. Directory identity
checks require a native file ID and directory creation time. On Linux, capture
fails when the filesystem does not provide birth-time metadata. Windows reads a
128-bit file ID from an open handle. Identity records alone are configuration
metadata. The v0.3.0 core also authorizes a bounded native file broker, exposed
only through trusted CLI commands. Broker operations add live handle checks and
use a separate file-operation journal. The v0.4.0 core adds a session-owned
turn/tool runtime and schema-version-2 journals. That runtime is exercised with
a scripted model and the real control store, broker, and temporary workspace.
Since 0.12.0, CLI chat tests also drive it through the LM Studio adapter against
a local mock server. The v0.5.0 CLI adds a
read-only edit preview that compares a broker-read file with trusted proposed
content and never writes or checkpoints. These features do not provide an
execution sandbox. The 0.13.0 platform crate adds an unexposed process-tree
runner core; its tests re-execute the unit-test binary as the child program.
The 0.14.0 Linux confinement tests run that child under Landlock and seccomp
and record each probe's errno: Landlock denials report `EACCES`, and socket
denials report `EPERM`. A classic-BPF interpreter in the unit tests evaluates
every filter branch. These tests ran on WSL 6.6 with Landlock ABI 3; they do not
qualify other kernels. The 0.15.0 Windows tests copy the test binary into a
granted directory, run it in a fresh AppContainer, and record each probe as
`ok`, `denied`, or the OS error. The same probes run unconfined as a positive
control. Grant tests change real ACLs on temporary NTFS directories, and each
test container deletes its profile when dropped, so runs leave no per-user
AppContainer profiles. These tests ran on Windows 11 build 26300.

The 0.16.0 `run_workflow` tests drive `omgcode run` through the real CLI with
system tools. Since 0.17.0, on Linux they start the CLI through
`systemd-run --user --scope -p Delegate=yes`, and the platform tree-limit
tests rerun themselves the same way. They need a user service manager and fail
without one; they must not be skipped. Each limit test must show that the
limit ends the whole run as `LimitExceeded`. Since 0.19.0 the disk test writes
past a 16 MiB limit and expects the `disk` result; the platform tests also
exercise the sampler directly, including, on Unix, a link that must not be
followed and a ceiling reached during a measured scan. Windows scenes grant
the workspace and revoke every grant when dropped. The `toolchain_acceptance` tests run real toolchains. They are
`#[ignore]` because they need host toolchains named by `OMGCODE_ACCEPT_*`
variables. Run them explicitly with `--ignored` on both platforms and report
each language that did not run. A missing variable fails the case rather than
passing it. See [workspace permissions](workspace-permissions.md)
and the [workspace file-access contract](file-access.md) for their scope and
current limits. [Repository review and analysis](repository-checks.md) have
separate workflows.

The directory-record unit tests cover native path encoding, record JSON shape,
stability after child writes, same-path replacement, and ancestor/sibling
observations. They also cover missing creation-time metadata and reject opening
a FIFO as a directory. Control-store tests cover strict journal replay, stale
revision rejection without a write, workspace ID reuse and replacement, moved
or stale directory rejection, bindings, audit records, and lock contention.
CLI workflow tests bound subprocess waits and captured stream reads, and assert
the exact number of durable `policy_checked` events in the tested workflow.
They exercise configuration commands and confirm that policy checks remain
configuration-only. File broker tests cover bounded reads, listings, exclusive
creation, scope denials, path/link checks, collisions, audit privacy, and
uncertain-create recovery. Edit-preview unit and CLI tests cover exact hunk
rendering, unchanged content, CRLF and final-newline preservation, line-count
and diff bounds, binary and oversized refusal, proposed-source link rejection,
permission denial, and absence of workspace mutation. Turn-runtime tests cover allowed read/list through a
fresh live binding, policy denial at the broker, revocation during model I/O,
unknown and malformed calls, duplicate identifiers, per-round, aggregate,
prompt-capacity, and selected per-message limits, cancellation, deadline expiry,
archived and active-turn refusal, workspace metadata mismatch, model-addressable
listing names, interim assistant text retention, source-labelled hostile
content, non-UTF-8 withholding, and schema-v1/v2 replay including tool
output-identity checks and failed-turn call-ID reuse. Public integration tests exercise these operations
through the broker API; core unit tests inject the uncertain outcome. These
tests do not qualify release filesystem support or runtime containment.

Use the locked Cargo commands below. They are also configured in
`.github/workflows/rust.yml` for Ubuntu 24.04 and Windows Server 2025. A CI
matrix or passing test suite does not establish release support or sandbox
containment.

Documentation-only changes need documentation checks. Do not report application
tests for documentation-only work.

## Behaviour and regression tests

- Use native Rust `#[test]` functions and, when an async runtime is selected,
  its supported test attribute. A separate BDD framework is not required.
- Name each test for an observable behaviour and its relevant condition.
  Arrange setup, action, and assertions in Given/When/Then order. Short comments
  can label phases; Rust does not require Catch2-style macros.
- Check exact outputs, errors, side effects, and persistent state. Include
  boundary and failure cases. Do not test only a private helper's chosen shape.
- For a regression, add the failing behaviour test before the production fix.
  Observe its failure on the unfixed baseline when safe and practical. Record
  a limitation if the failure cannot be reproduced; do not weaken the assertion.
- Put isolated unit tests beside the module, normally in `#[cfg(test)] mod tests`.
  Put cross-component or executable tests in the owning crate's `tests/`
  directory. Keep reusable support separate from test entry points.
- Use a real library target when integration tests need a reusable core.
  Do not make internal functions public merely to test implementation details.
- Executable documentation examples must compile and run as doc tests when
  applicable. Mark genuinely illustrative pseudocode clearly.

## Isolation and concurrency

- Give tests private temporary directories, repositories, databases, ports, and
  bounded resources. Do not use real home folders or persistent credentials.
- Avoid process-global environment or working-directory mutation in parallel
  tests. Pass explicit configuration or isolate the case in a subprocess.
- Use barriers, channels, deterministic clocks, and injected failures for
  concurrency. Arbitrary sleeps are not evidence of ordering.
- A Unix child can inherit a test-held `flock` until `exec`, so the lock can
  outlive the test's close. CLI integration tests must open lock-holding stores
  inside `without_child_spawns` and start children through the spawn gate in
  `crates/omgcode-cli/tests/support/spawn_gate.rs`. Do not retry on `Busy`, which
  would hide real contention. Serial runs cannot detect this race, so run the
  default parallel suite as well.
- Test cancellation before start, during work, during partial writes, and during
  shutdown. Observe task completion, resource release, and descendant cleanup.
- Bound every waiting integration test. A timeout is a failure, not a skip.
- Cross-compilation and mocks cannot establish operating-system containment.
  Run actual filesystem, process-tree, IPC, and network tests on both Windows
  and Linux before claiming those properties. Workspace identity tests are
  configuration checks only. The native broker adds bounded filesystem
  operations but does not qualify release filesystem support or runtime
  containment. The [workspace permission contract](workspace-permissions.md)
  and [file-access contract](file-access.md) define their scopes and remaining
  platform checks.

## Required boundary coverage

As the relevant components are implemented, include:

| Area | Required cases |
| --- | --- |
| Permissions | Allowed/denied requests, stale grants, scope mismatch, revocation; workspace registration and directory replacement |
| Workspace identity | Native paths, stable directory IDs after child writes, ancestor/sibling checks, missing creation-time metadata failure |
| Files | Bounded read/list/create/mkdir, policy scope, traversal, native paths, symlinks/junctions, hard links, special files, collisions, uncertain creation, audit privacy, path-swap races, and read-only edit-preview bounds/no-mutation |
| Processes | Descendant escape attempts, limits, cleanup, startup failure |
| Disk limits | Sampled growth ends the whole run as `disk`, baseline excludes prior content, links and reparse points are not followed, the ceiling stops a scan early, unreadable or oversized roots fail closed |
| Networking | Disabled egress, host-local access, redirects, provider routing |
| Package network | Profile storage and the trusted fetch: recording, replay, revision, origin normalization, duplicate and invalid-origin refusal, removal, task-network gating, undeclared-origin and path refusal before a request, redirect refusal, size limits, cache-name validation, existing-cache refusal, private cache content, and start/finish audit without the URL text. Cache roots cover per-workspace set/clear/replay, overlap refusals, the configured fetch location, Linux read/write/parent denial, Windows access with a scoped grant, and Windows refusal without one. No run accepts a profile. A future run-side mechanism must add allowed-origin success, undeclared-origin refusal, redirect and credential refusal, offline fallback, and platform refusal ([ADR-0034](adr/0034-gate-package-networking-behind-enforceable-destination-control.md), [ADR-0035](adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md), [ADR-0037](adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md)) |
| Providers | Partial/malformed streams, duplicate calls, interruption, retries |
| Provider profiles | Kind/authentication pairing, duplicate and invalid names, case-insensitive lookup, revisioned add/remove, replay, no credential or endpoint storage, and capability reporting. No adapter or login is exercised |
| Approvals | Request validation, pending/list/show, approve/deny, single-use consume, expiry, binding mismatch, settings-revision invalidation, replay, unknown IDs, and limits. No operation executes |
| Authentication | Callback binding, consent, subscription entitlement, refresh, revocation, account/billing isolation |
| Toolchains | Native compiler/SDK dependencies, interpreters, formatter/build/test flows, caches, helper processes, denied host access |
| Git | Hostile config/hooks/helpers, metadata paths, changed push approvals |
| Persistence | Partial operations, migration failure, recovery without replay |
| Frontend | Untrusted rendering, IPC authorization, no secret disclosure |
| Sessions | Concurrent edits/worktrees, JSONL naming/schema/durability, index rebuild, archive/restore, safe resume, v1/v2 boundaries |
| Turn/tool lifecycle | Fresh bindings, strict schemas, unknown/malformed/duplicate calls, round/call/byte limits, cancellation, deadline, source labels, denial at the broker, no replay |
| Process-tree runner | Exit codes, exact environment, argument quoting, bounded output, deadline, cancellation, descendant escape attempts, inherited handles, observed cleanup, invalid specifications |
| Linux confinement | Read-only and read-write rules, path truncation, denied host files and directories, inheritance by descendants, IP and `io_uring` denial in both Unix-socket modes, Unix-socket and datagram-pair denial in `PairsOnly`, host Unix-socket connection, workspace socket binding, and datagram pairs in `HostServices`, unexecutable programs, missing/link/special confinement paths, ABI selection below and above the minimum |
| Windows confinement | Granted read-only and read-write paths, denied host files, directories, named pipe, and loopback TCP, a writable container temporary directory, inheritance by descendants, the same probes unconfined, missing, weaker, and foreign grants, a missing profile, `LOCALAPPDATA`, or container name, refused `HostServices`, an ungranted program |
| Container grants | SID derivation, propagation to existing and new children, exact read-back, protection state kept, junctions not followed, hard-link reach, refused replaced/junction/file/relative paths, profile creation and deletion, journal intent and outcome order, replay validation, revision rules, scope refusals, pending and revoking recovery, the revocation reserve, CLI grant/list/revoke, refusal off Windows |
| Model tool chat | Preference off by default and rechecked per call, settings compatibility, confinement against parent, absolute, rooted, UNC, link, junction, and hard-link paths, strict provider tool-call parsing, token removal before journal and model, refusal of token-bearing calls, withheld replay after revocation, finish reason, one call per turn in escape cases |
| Model command tool | Strict profile/program/argument/cwd parsing, tool advertised only with live model-tools/execute/read/write, first call records a pending approval and starts nothing, denial, approval consumption, exact digest and binding, settings-revision invalidation, run hand-off failure, output bounds, and no replay. An end-to-end CLI case drives the real chat process, a mock provider, an operator approval, and the trusted runner to a successful confined command on both hosts |
| MCP | Version/transport compatibility, authorization, untrusted content, local confinement, OAuth/redirects, cancellation |
| LM Studio | Model discovery, token/no-token profiles, streamed tools, context limits, unavailable models, no hosted-tool bypass |
| Instructions | Compiled base/UI/request match, runtime replacement denial, role-injection attempts, preference/task precedence, revisions, safe rendering |
| Compaction | Token thresholds and smaller limits, summary injection, concurrent steering, tool-pair integrity, bounded failure |
| LLM caches | Content/policy/account isolation, revocation, stale/poisoned entries, quota/clear, no side-effect replay |
| Skills | Package/link validation, scope overrides, disabled loading, script containment, live disable and context invalidation |
| Memories | Explicit project/global choice, identity isolation, lifecycle, injection, stale facts, bounded retrieval, invalidation, no replay resurrection |

Use deterministic local provider mocks in normal tests. Live-provider tests must
be explicit opt-in, use disposable data and a budget, and report which endpoint
and model were tested. A mock pass is not a live-provider compatibility result.

The current LM Studio adapter tests use a local HTTP fixture. They do not call a
live model. The adapter's text method sends one non-streaming Chat
Completions request and rejects any tool call in the reply. It requires the
compiled base prompt and accepts only a literal loopback HTTP endpoint. The
adapter is not a permission boundary: the CLI selects tool mode from the live
`model_tools` preference, and the runtime rechecks that preference before each
call. An end-to-end acceptance case starts the real chat CLI under a delegated
cgroup on Linux, has the mock provider request the platform shell's `echo`,
approves the recorded call between turns, and asserts that the second identical
turn consumes the approval and runs the command through the trusted runner to
an `exited` result with code 0 and the expected output. On Windows the scene
grants the workspace. The case needs those host facilities and fails without
them; it does not qualify confinement correctness, which the platform suites
own. It passed on Windows 11 build 26300 and WSL 6.6 (Ubuntu 24.04) on
2026-11-14. The turn-runtime tests use a scripted model interface. The tool-enabled chat tests
use a mock that returns the documented OpenAI-compatible tool-call shape; they
do not establish live LM Studio tool-call compatibility. A single opt-in spot-check
with LM Studio 0.4.28 and `google/gemma-4-e4b` on Windows is recorded in the implementation plan. Model discovery, streaming, cancellation, and
live model compatibility remain future qualification work. The CLI can inspect
and archive sessions, but it does not execute recorded tool actions or resume
an incomplete turn.

The [authentication contract](provider-authentication.md) adds subscription
acceptance gates; the [toolchain contract](development-tools.md) defines real
development workflows to qualify on each supported OS. Documentation checks
cannot establish login entitlement or compiler compatibility.

The [instruction](agent-instructions.md), [context](context-management.md), and
[skills](skills.md) guides own detailed acceptance cases. Include malicious
instructions in direct and indirect content, including compaction summaries and
cached artifacts. A model refusing an attack is not proof that the core denied
an unauthorized action. Inspect the actual execution and disclosure boundaries.
Measure efficiency on useful tasks, including compaction/cache overhead and task
correctness; do not infer savings from cache configuration alone.

The [MCP](mcp.md) and [session storage](session-storage.md) guides add connection,
archive, persistence, corruption, and recovery cases. Run real process/file tests
on both platforms before claiming containment or durability. Parser/example
checks are not those runtime tests. Qualify LM Studio's endpoint/model/auth mode
separately; OpenAI compatibility is not proof of complete OpenAI API behaviour.
The [memory contract](memories.md) adds scope/lifecycle cases. Check both allowed
global reuse and denied project-memory disclosure; a successful retrieval alone
does not establish correct scope or current factual accuracy.

## Cargo verification baseline

The workspace pins Rust 1.99.1 and commits `Cargo.lock`. Run from its root:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

For workspace library targets, run documentation tests separately:

```text
cargo test --workspace --doc --locked
```

Cargo's `--all-targets` covers libraries, binaries, tests, benches, and examples;
it does not include doc tests. Review the manifest's target settings and required
features to confirm that the intended tests actually execute.

Run declared supported feature configurations separately, including defaults.
Do not blindly use `--all-features`: optional backends or platform features can
form unsupported combinations. Explicitly document any excluded combination and
its reason. Test a claimed minimum Rust version separately when it differs from
the pinned development toolchain.

The frontend does not exist. Add its actual type, lint, rendering, and link
checks here when it is introduced. Do not report nonexistent gates as active.

## Evidence and completion

Record exact commands, toolchain, platform, feature selection, completed exit
statuses, test counts, ignored cases, filtered cases, and timeouts. Investigate a
targeted filter that executes zero tests. A legitimately empty suite is not
coverage of the requested behaviour.

A build still running, compile-only run, startup abort, crash, or incomplete log
is not a passing test run. Check the actual final summary and process result.
Tests ignored for a specific job need a stated reason and a job that exercises
the required behaviour where applicable.

Re-run the changed or new test directly and confirm it executed. Inspect
delegated results yourself. Broaden checks when changed dependencies or unresolved
risks justify it, and disclose checks that could not run.

Rustfmt, Clippy, property tests, fuzzing, sanitizers, and Miri each cover limited
properties. Use suitable tools for the risk, with a functioning harness and real
execution. Do not treat a clean report from an aborted tool as a pass.

## References

- [Cargo test targets and doc tests](https://doc.rust-lang.org/cargo/commands/cargo-test.html)
- [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)
- [Clippy in CI](https://doc.rust-lang.org/clippy/continuous_integration/)

## File-broker review regressions

Run the Linux pinned-read path-swap regression with its channel checkpoint;
static pre-operation replacement tests do not exercise that interval.
The Linux removed-entry and real permission-refusal tests must also execute.
The permission fixture expects an unprivileged user whose directory mode checks
are enforced; privilege bypass is a failure, not a silent skip.

The review regression target checks that successful and capacity-refused file
requests leave settings bytes unchanged and that permission revocation still
succeeds. Archive tests cover raw-byte preservation, full logs, unresolved-state
refusal, destination collisions, directory-sync faults, read-only inspection,
and continuation with a fresh active log. The CLI workflow checks recovery
commands after a development-root change.

## Replacement acceptance gates (planned)

Phase A2a has read-only initialization/ledger record tests through the public
core API. They cover complete/incomplete histories, ordering, acknowledgements,
scope/UUID/UTC validation, strict JSON, descriptors, raw native paths, and exact
byte/count bounds. A separate executable doc example checks the initialization
API. They do not test native checkpoint persistence or workspace replacement.

Phase A2b adds native Linux private-store and pinned source-capture tests,
core `checkpoint_storage` integration tests, CLI `checkpoint_recovery` tests,
and instance-local fault injection in the private writer. They cover exact
bytes and modes, source metadata restrictions, strict inventory, malformed or
missing artifacts, live authority, unchanged targets, reserved tail capacity,
64 retained intents, actual-file aggregate quota, initialization interruptions,
partial writes, sync uncertainty, acknowledgement, and existing-only reopen.
Recovery output excludes payloads. Inspection and acknowledgement offer no
apply or restore path; Phase A4 adds restore only as a separate command.
The tests use actual temporary files. They do not simulate physical power loss,
foreign-user attacks, privileged bind mounts, or qualify a release filesystem.
No A2b test demonstrates native replacement or metadata publication.

Phase A2c adds native Linux ext4 publication and CLI acceptance tests. Exercise
exact content and metadata, a new inode, retained checkpoints, final target and
stage checks, non-CAS interleavings, authority refusal, and one native attempt.
Test failures at the commit, verification, file sync, parent sync, and terminal
append boundaries. Reopen actual files to distinguish unchanged targets,
published targets, complete visible outcomes, and malformed ledger tails.
Use ext4 for Linux temporary targets and private storage. The native cross-mount
case also uses a disposable directory under `/dev/shm` on tmpfs. Tests do not
establish physical power-loss behavior.

Phase A3b runs the capture, storage, and apply tests natively on Windows NTFS.
Exercise exact security descriptors, each refused metadata kind, sharing
conflicts, the step order, and post-rename failures. A recheck test must defeat
the earlier checks before it can show a later one works: restore the file times
after changing security, streams, or the name, and assert that the retained
snapshot still matches. Check that the stage keeps its owner-only DACL through
preparation, rechecks, conflicts, and failures before the rename, even when the
source grants other principals. Inject a failed privacy restore, and recreate the
crash state by copying the exact source descriptor onto a retained stage: recovery
must accept it after commit intent and refuse it before. Write the source DACL
and the copy through the same API and assert the copy matches exactly: .NET
`FileSecurity` reorders access entries, and an elevated runner adds its own
default entries. CI temporary files inherit no entries, so a test that needs
inherited entries must create them. To reproduce that locally, point `TMP` and
`TEMP` at a directory without inheritable entries. Tests do not cover audit
SACLs, a foreign owner,
a second local user, a real second volume, ReFS, filter drivers, or physical
power loss.

Phase A4 adds restore and checkpoint-deletion tests in the core replacement
module, the `replacement_records` ledger tests, the platform removal tests, and
CLI `checkpoint_lifecycle` tests. Run them on native Windows NTFS and Linux ext4.
Restore tests must show stale-content conflicts, foreign or missing checkpoints,
denied authority, preserved security metadata, and a new checkpoint holding the
replaced bytes. Deletion tests must show that unresolved operations are refused,
that history survives deletion, that one retained slot is freed without
eviction, and that unknown names and hard links are never removed. Inject a
failure after each intent, artifact, directory, and outcome boundary, and show
that only the same request finishes an interrupted deletion. On Windows, cover a
stage with a foreign DACL, a read-only file, and a refused read-only file whose
DACL denies attribute writes. Tests do not establish power-loss behavior.

Before enabling another native replacement backend, implement the
[accepted replacement contract](file-access.md#replacement-contract)
and run the remaining acceptance cases on native Windows and Linux. Use private,
disposable files. Use barriers or injected failures at the actual stage/check/
commit/sync boundaries; sleeps and pre-operation path swaps are insufficient.

| Boundary | Required evidence |
| --- | --- |
| Admission | Missing/expired/stale bindings, wrong session/workspace, read/write denial, changed policy, full logs, and quotas refuse replacement. Settings and permission revocation remain usable. |
| Preconditions | Stale digest, stale size, and a changed entry observed before commit intent cause a controlled conflict, including swaps with equal bytes. A mismatch after intent stops the attempt and stays unresolved. Reject invalid UTF-8 and unsafe proposed-source objects. Equal proposed bytes cause no replacement or checkpoint. Check exact line endings and byte counts. |
| Concurrent change | In-place writes and entry swaps with mismatches observable at the final check produce conflicts and leave workspace entries unchanged by the broker. Preserve the independent edit. An A-to-B-to-A write can go undetected when all checked metadata matches again. Exercise that case and changes after the check; report the documented non-CAS limit rather than asserting isolation. |
| Native objects | Links, junctions/reparse points, hard links, devices, mount crossings, invalid native paths, unsupported metadata, and cross-filesystem staging refuse replacement. Verify stage and parent substitution checks. |
| Checkpoints | Exact prior/proposed bytes and digests, exclusive-name collisions, private modes/ACLs, 1 MiB payload bounds, 64 KiB manifests, 64 entries, aggregate 64 MiB quota, and bounded unknown-layout refusal. Count incomplete artifacts; do not evict evidence. |
| First use | Upgraded installations with no replacement state keep ordinary reads usable without creating replacement files. Interrupt each initialization/sync boundary. Validate the bounded initializing/ready anchor, reject orphaned or partial stores, and refuse to recreate a missing ledger after initialization. Settings changes remain usable. |
| Publication | Observe a prepared durable checkpoint before the commit attempt. Preserve required security metadata. Assert one native commit attempt and verify both checkpoint payloads remain intact. No truncate, copy/delete, or weaker fallback. |
| Failure and reopen | Inject failures at every intent, payload, manifest, stage, native commit, file/directory sync, outcome, and acknowledgement boundary. Inspect the final ledger and disk state. Post-commit uncertainty never becomes clean failure or success without evidence. |
| Recovery | Reopen never replays work. Missing/corrupt/foreign artifacts fail closed. Inspection is existing-only, acknowledgement does not repair, and caps preserve reserved recovery capacity. Verify no automatic stage deletion or rollback. |
| Restore boundary | Verify inspection and acknowledgement expose no restore path. Test restore's fresh read/write authority, explicit expected current bytes, detection of later user changes, a new checkpoint preserving them, refusal of foreign, deleted, or pending checkpoints, and missing-target restore-as-create refusal. |
| Checkpoint deletion | Require an explicit permanent acknowledgement. Refuse pending, unresolved, deleted, and unknown operations. Record intent before removal and keep history afterward. Remove only fixed names, one entry at a time, without recursion, links, or ACL repair. Interrupt every boundary; only the same request may finish it, and other work stays blocked meanwhile. |
| Compatibility and privacy | Existing settings, file-operation-v1, and session journals remain unchanged. New admission checks both logs. Downgrade remains unsupported while replacement state exists. Keep checkpoint contents out of audit output, model context, and conversation events. |

A passing rename test does not establish checkpoint power-loss durability,
Windows directory-option support, ACL preservation, or a conflict-free edit.
Record the host OS/filesystem, verified API requirements, and remaining gates
before marking a backend available. Apply the normal completed-result and
targeted-test requirements above. Existing create/read tests are not evidence
that replacement or restore works.
