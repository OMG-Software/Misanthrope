# Testing standards

Use these standards with the [coding rules](coding-rules.md) and
[security rules](security-coding-rules.md).

## Current state

The pinned Cargo workspace has three crates: `orangecode-core`,
`orangecode-cli`, and `orangecode-platform`. It includes deterministic core,
CLI, and platform tests. The core includes configuration-only workspace records,
permission preferences, and a local settings/audit journal. Directory identity
checks require a native file ID and directory creation time. On Linux, capture
fails when the filesystem does not provide birth-time metadata. Windows reads a
128-bit file ID from an open handle. Identity records alone are configuration
metadata. The v0.3.0 core also authorizes a bounded native file broker, exposed
only through trusted CLI commands. Broker operations add live handle checks and
use a separate file-operation journal. They do not provide model tool dispatch,
a process runner, or an execution sandbox. See [workspace permissions](workspace-permissions.md)
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
uncertain-create recovery. Public integration tests exercise these operations
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
| Files | Bounded read/list/create/mkdir, policy scope, traversal, native paths, symlinks/junctions, hard links, special files, collisions, uncertain creation, audit privacy, and path-swap races |
| Processes | Descendant escape attempts, limits, cleanup, startup failure |
| Networking | Disabled egress, host-local access, redirects, provider routing |
| Providers | Partial/malformed streams, duplicate calls, interruption, retries |
| Authentication | Callback binding, consent, subscription entitlement, refresh, revocation, account/billing isolation |
| Toolchains | Native compiler/SDK dependencies, interpreters, formatter/build/test flows, caches, helper processes, denied host access |
| Git | Hostile config/hooks/helpers, metadata paths, changed push approvals |
| Persistence | Partial operations, migration failure, recovery without replay |
| Frontend | Untrusted rendering, IPC authorization, no secret disclosure |
| Sessions | Concurrent edits/worktrees, JSONL naming/schema/durability, index rebuild, archive/restore, safe resume |
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
live model. The adapter supports one non-streaming Chat Completions request,
rejects tool requests, requires the compiled base prompt, and accepts only a
literal loopback HTTP endpoint. Model discovery, streaming, cancellation, and
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
