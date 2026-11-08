# Testing standards

Use these standards with the [coding rules](coding-rules.md) and
[security rules](security-coding-rules.md).

## Current state

There is no Cargo manifest, frontend project, or application test suite yet.
[Repository review and analysis](repository-checks.md) have separate workflows.
The Cargo commands below define the baseline to introduce with implementation;
they are not current working installation or verification instructions.

Documentation-only changes need documentation checks, not an invented Rust
project or a claimed application test run.

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
  and Linux before claiming those properties.

## Required boundary coverage

As the relevant components are implemented, include:

| Area | Required cases |
| --- | --- |
| Permissions | Allowed/denied requests, stale grants, scope mismatch, revocation |
| Files | Traversal, native paths, symlinks/junctions, hard links, path-swap races |
| Processes | Descendant escape attempts, limits, cleanup, startup failure |
| Networking | Disabled egress, host-local access, redirects, provider routing |
| Providers | Partial/malformed streams, duplicate calls, interruption, retries |
| Authentication | Callback binding, consent, subscription entitlement, refresh, revocation, account/billing isolation |
| Toolchains | Native compiler/SDK dependencies, interpreters, formatter/build/test flows, caches, helper processes, denied host access |
| Git | Hostile config/hooks/helpers, metadata paths, changed push approvals |
| Persistence | Partial operations, migration failure, recovery without replay |
| Frontend | Untrusted rendering, IPC authorization, no secret disclosure |
| Sessions | Concurrent edits, separate worktrees, shared repository state |

Use deterministic local provider mocks in normal tests. Live-provider tests must
be explicit opt-in, use disposable data and a budget, and report which endpoint
and model were tested. A mock pass is not a live-provider compatibility result.

The [authentication contract](provider-authentication.md) adds subscription
acceptance gates; the [toolchain contract](development-tools.md) defines real
development workflows to qualify on each supported OS. Documentation checks
cannot establish login entitlement or compiler compatibility.

## Cargo verification baseline

After a Cargo workspace, toolchain pin with rustfmt and Clippy components, and
lockfile exist, run from its root:

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

When frontend and documentation-build tooling exists, add its actual type,
lint, rendering, and link checks here. Do not report nonexistent gates as active.

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
