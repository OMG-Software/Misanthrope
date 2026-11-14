# 0031: Limit whole run trees with job objects and delegated cgroups

- Status: Accepted
- Date: 2026-11-12

## Context

[ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md)
added the trusted CLI `run`. Its runs had a wall-clock deadline and output
caps, but no limit on memory, process count, or CPU time. One run could
exhaust the host's memory or fork until the user's process limit was reached.
Increment C5 adds tree-wide limits.

The user chose these options for C5:

- **Linux mechanism:** a delegated cgroup v2. The operator starts OMGCode in a
  scope of its own, such as `systemd-run --user --scope -p Delegate=yes`, and
  the runner creates a child cgroup for each run. No new dependency.
- **Posture:** always, fail closed. Every trusted run is limited. A Linux run
  outside a delegated cgroup is refused before it is recorded.
- **Limits:** memory, process count, and CPU time.
- **Configuration:** `run` flags with defaults, recorded in the run journal.

Facts observed during the work, on Windows 11 build 26300 and WSL 6.6:

- Under WSL, a login shell and the test harness run in `/init.scope`, which
  belongs to root. A `systemd-run --user --scope -p Delegate=yes` scope belongs
  to the user and offers `memory` and `pids`.
- cgroup v2 refuses to enable controllers for the children of a cgroup that
  holds processes. The runner must first move itself into a leaf.
- Job-object limit messages arrive on an I/O completion port; Windows does not
  guarantee their delivery.

## Decision

Every trusted run carries `TreeLimits`: memory in bytes, live processes, and
CPU time. The CLI defaults are 8 GiB, 1024 processes, and 2 hours; the ranges
are 16 MiB to 1 TiB, 1 to 65,536, and 1 second to 7 days. The core refuses a
request without limits. The [execution guide](../execution.md#tree-limits)
owns the details.

- **Windows:** the run's existing job object gets `JobMemoryLimit` and
  `ActiveProcessLimit`, with a completion port for limit messages. CPU time
  comes from the job's basic accounting.
- **Linux:** the runner checks that its own cgroup is a delegated v2 cgroup
  that belongs to the user and offers `memory`, `pids`, and `cgroup.kill`. It
  moves into an `omgcode-supervisor` leaf, enables the controllers for the
  children, and creates one cgroup per run with `memory.max`,
  `memory.swap.max` 0, `memory.oom.group` 1, and `pids.max`. The child joins
  it before `exec`. CPU time comes from `cpu.stat`. Termination uses both the
  process group and `cgroup.kill`, and emptiness also needs
  `populated 0`.
- **Semantics:** reaching any limit ends the whole tree. The result is
  `limit_exceeded` with the limit, and it takes precedence over the program's
  own exit. The supervisor checks CPU time at each poll.
- **Records:** `run_started` records the limits and `run_finished` the CPU
  time and, where reported, the peak memory. Older records without these
  fields still replay.

## Alternatives considered

- **`setrlimit` on Linux.** `RLIMIT_AS`, `RLIMIT_NPROC`, and `RLIMIT_CPU` are
  per process or per user, not per tree. A tree could exceed them by forking,
  and `RLIMIT_NPROC` counts the user's unrelated processes. Rejected.
- **A systemd unit per run.** `systemd-run` per run, or the D-Bus API, would
  add a dependency on the service manager's interface and its timing for every
  run. Rejected in favour of one delegated scope.
- **Opt-in or best-effort limits.** Running without limits when the platform
  cannot apply them hides the gap from the operator. The user chose to fail
  closed.
- **A kernel CPU limit on Windows.** `PerJobUserTimeLimit` counts user time
  only and ends the job without a distinct result. Polling the accounting
  counts kernel time too and lets the runner record the reason.

## Consequences

- Each run is capped at its configured memory, process, and CPU-time limits.
  The limits reserve nothing: a run can still exhaust a host with less free
  memory or process capacity than they allow, and concurrent runs each get
  their own limits.
- Linux runs need a user service manager or another delegated cgroup, and the
  scope must not hold other processes. Hosts without cgroup v2, or older than
  Linux 5.14, cannot run. CI must provide a user systemd instance.
- The limits mean different things on each platform: committed memory on
  Windows, cgroup memory including page cache on Linux; Linux counts threads.
- CPU time can overshoot by one poll's worth of time per busy thread.
- A lost Windows limit message leaves the limit enforced but the result
  unattributed. Windows routes a nested job's limit messages to its
  ancestors, so the runner counts a memory or process message only when the
  run's own job confirms it. A large allocation refused well below the memory
  limit is then not attributed either. No documented API identifies the job
  that sent a message, so a nested job's memory message still ends a run that
  is already within a sixteenth of its own memory limit.
- There is still no disk limit. ADR-0033 later added a sampled net disk-growth
  limit; that is not a kernel quota.

## References

- [Execution contract](../execution.md#tree-limits)
- [ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
- [ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md)
- [ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md)
- Linux `Documentation/admin-guide/cgroup-v2.rst`: delegation, the
  no-internal-process rule, `memory`, `pids`, and `cgroup.kill`
- Windows `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` and
  `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`
