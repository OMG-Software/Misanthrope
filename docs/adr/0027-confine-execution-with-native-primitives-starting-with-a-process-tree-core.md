# 0027: Confine execution with native primitives, starting with a process-tree core

- Status: Accepted
- Date: 2026-11-11

## Context

Phase C adds command execution. The [development-tool contract](../development-tools.md)
and [security rules](../security-coding-rules.md#process-and-sandbox-boundary)
require confinement before untrusted code starts, limits on descendants,
controlled networking, observed process-tree cleanup, and no fallback to
unrestricted execution.

That is too large for one reviewable change. Each platform needs a different
confinement primitive, and some choices affect the user's files and hosts:

- On Linux, the test kernel (WSL 6.6) offers Landlock ABI 3 and unprivileged
  user namespaces. Landlock ABI 3 has filesystem rules but no network rules.
  Stock Ubuntu 24.04 is reported to restrict unprivileged user namespaces
  through AppArmor; this was not verified on a non-WSL host.
- On Windows, AppContainer gives filesystem and network isolation, but the
  workspace must grant the container identity access through its ACL.

## Decision

The user decided:

- Linux confinement uses **Landlock and seccomp**.
- Windows confinement uses **AppContainer inside a Job Object**.
- Trusted operator CLI commands are the **first caller**. A model-callable
  command tool needs a separate decision.
- The **first increment (C1)** is the execution contract and a process-tree
  runner core. It is not exposed to the CLI or the model.

C1 implements `omgcode_platform::process::run`. It starts one program from an
absolute path with an exact environment, null input, and bounded output. It
terminates the tree on exit, deadline, or cancellation, and reports success only
after observing the tree empty. The [execution guide](../execution.md) owns the
contract, limits, and platform mechanics:

- **Windows:** a suspended `CreateProcessW` with the job assigned at creation by
  attribute list, a handle list for inheritance, no breakaway, and
  kill-on-close.
- **Linux:** a new process group, a seccomp filter denying `setsid` and
  `setpgid`, a parent-death signal, `no_new_privs`, and close-on-exec for
  inherited descriptors.

## Alternatives considered

- **User namespaces (bubblewrap-style) on Linux:** strong isolation and a PID
  namespace for tree control, but unavailable wherever unprivileged user
  namespaces are restricted. Rejected by the user in favour of Landlock and
  seccomp.
- **Requiring both namespaces and Landlock:** strongest, but fails closed on the
  most hosts. Rejected.
- **Restricted token instead of AppContainer on Windows:** needs no grants for
  reads, but has no native network block, and write confinement still needs
  integrity-label changes. Rejected.
- **`std::process::Command` on Windows:** it cannot assign a job at creation or
  restrict inheritance to a handle list. Assigning a job after spawn lets the
  child start an escaping descendant first. Rejected.
- **cgroup v2 for Linux tree control:** gives tree-wide kill and limits, but
  needs a delegated, writable cgroup that unprivileged sessions often lack.
  Deferred; the process group with seccomp works unprivileged.
- **Exposing a model command tool first:** rejected by the user; trusted CLI use
  comes first.
- **Shipping confinement and exposure in one change:** too large to review and
  verify on both platforms. Rejected in favour of increments C1–C4.

## Consequences

- No command can run through OMGCode yet. The runner core has no confinement,
  so later increments must add it before any exposure. `execution_allowed`
  remains false.
- Linux needs kernel 5.11 or later for `close_range`, seccomp filters, and an
  x86_64 or aarch64 host. Windows needs a version that supports
  `PROC_THREAD_ATTRIBUTE_JOB_LIST` (Windows 10 or later).
- Programs that call `setsid` or `setpgid` fail with `EPERM` on Linux.
- There are no tree-wide memory or process-count limits yet. On Linux,
  descendants survive a crash of the runner process itself.
- C3 will change workspace ACLs on Windows. That change must be visible,
  recorded, and reversible.
- Tests prove cleanup, escape denial, inheritance limits, quoting, and bounded
  output on Windows and Linux. Positive-control mutations were each caught.
  These tests are not confinement or release qualification.

## References

- [Execution guide](../execution.md)
- [Development tools](../development-tools.md)
- [ADR-0003](0003-use-scoped-development-toolchain-profiles.md)
- [ADR-0012](0012-separate-workspace-settings-from-execution-authority.md)
