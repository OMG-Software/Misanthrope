# Command execution

This guide owns the execution contract: how OMGCode starts a program, bounds
it, and stops its whole process tree, and which native confinement each platform
must apply before a command can be exposed. The
[development-tool contract](development-tools.md) owns toolchain profiles; the
[security rules](security-coding-rules.md#process-and-sandbox-boundary) own the
general process boundary.

## Status

Version 0.13.0 adds only the **process-tree runner core** in the
`omgcode-platform` crate (`process::run`). It applies **no filesystem or network
confinement**. No CLI command, model tool, or core API calls it. Settings
reports keep `execution_allowed: false`. Do not describe it as a sandbox or as
command support.

The core exists so that later increments add confinement to a supervised,
tested tree instead of to an unbounded child process.

## Accepted decisions

The user selected these on 2026-11-11; see
[ADR-0027](adr/0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md).

| Area | Decision |
| --- | --- |
| Linux confinement | Landlock for filesystem access and seccomp for the rest. Both work unprivileged and do not need user namespaces. |
| Windows confinement | AppContainer for filesystem and network isolation, inside a Job Object for the process tree. |
| First caller | Trusted operator CLI commands only. A model-callable command tool needs its own later decision. |
| First increment | This process-tree core, unexposed, with tests on both platforms. |

Every later increment must fail closed. If a required primitive or rule cannot
be applied, nothing starts. There is never a fallback to unconfined execution.

## Runner contract

`process::run(spec, limits, cancel)` starts one program and returns only after
the whole tree is observed empty, or with an error.

Inputs:

- `program` must be an absolute path. The runner never searches `PATH`.
  On Windows it must be a plain `.exe` file. Batch scripts are refused because
  `cmd.exe` re-parses their arguments. Names ending in `.` or a space, stream
  names, quotes, and device paths are also refused.
- `arguments` are passed exactly. On Windows they are quoted for the Microsoft C
  runtime rules; programs that parse their command line differently are not
  covered.
- `working_directory` must be absolute.
- `environment` is the complete environment. Nothing is inherited. Names must be
  non-empty, unique (case-insensitively on Windows), and contain no `=` or NUL.
- Arguments plus environment are limited to 128 KiB, 4096 arguments, and 512
  environment entries. The Windows command line is limited to 32,767 UTF-16
  units.

Limits:

| Limit | Default | Maximum |
| --- | --- | --- |
| Deadline from start | 60 seconds | 1 hour |
| Retained stdout, retained stderr | 1 MiB each | 16 MiB each |
| Cleanup observation | 5 seconds | 30 seconds |

Behaviour:

- Standard input is the null device. Standard output and error are pipes.
  Output beyond the retained limit is drained and counted, so a writer never
  blocks; the result reports the total and whether it was truncated.
- The tree is terminated when the program exits, when the deadline elapses, or
  when cancellation is requested. Descendants never outlive a successful run.
- A run that returns `Ok` reports `Exited`, `Signalled` (Linux), `DeadlineExceeded`,
  or `Cancelled`. Each means the tree was observed empty and both output streams
  closed.
- Failures are typed. Invalid specifications and limits start nothing. `Spawn`
  leaves no process. `Supervision` is returned only after the tree was observed
  empty. `CleanupUnconfirmed` means a process or an open output handle remained
  after the cleanup timeout; it is never reported as success.

## Platform mechanics

### Windows

- `CreateProcessW` starts the program suspended. `PROC_THREAD_ATTRIBUTE_JOB_LIST`
  places it in a new Job Object at creation, so it cannot start a descendant
  outside the job. The runner checks membership with `IsProcessInJob` before
  resuming it.
- The job sets `KILL_ON_JOB_CLOSE` and `DIE_ON_UNHANDLED_EXCEPTION` and grants no
  breakaway. Closing the job handle, including on an error path, kills every
  member.
- `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limits inheritance to the null input and
  the two output pipe ends. Those ends are inheritable only while a module lock
  is held around `CreateProcessW`.
- Termination uses `TerminateJobObject`. Cleanup is observed when the job's
  active process count is zero. The pipes report end of file once all members
  are gone.

### Linux

- The program starts as the leader of a new process group.
- Before `exec`, the child:
  - sets `PR_SET_PDEATHSIG` to `SIGKILL` and confirms its parent is still the
    runner;
  - marks every descriptor above standard error close-on-exec with
    `close_range` (Linux 5.11 or later);
  - sets `no_new_privs`;
  - installs a seccomp filter that makes `setsid` and `setpgid` fail with `EPERM`
    and kills the process for any foreign ABI (32-bit or x32).
- Descendants inherit the filter, so none can leave the process group.
  Termination sends `SIGKILL` to the group. Cleanup is observed when signal 0 to
  the group reports no such process.
- Supported architectures are x86_64 and aarch64. Others return `Unsupported`.

## Known limits

These remain true after C1 and must be addressed or documented by the increments
that expose execution:

- **No confinement.** A program can read and write anything the user can, use
  the network, and talk to host services.
- **No tree-wide resource limits.** There is no memory, process-count, or disk
  limit. Windows can add job limits; Linux needs a delegated cgroup or another
  reviewed mechanism.
- **Linux runner crash.** `PR_SET_PDEATHSIG` reaches only the direct child.
  If the runner process dies, descendants keep running until something else
  stops them. Windows kills the job when the runner's handle closes.
- **Linux reaping.** Killed descendants are reaped by the system's subreaper or
  init. Where nothing reaps orphans, cleanup is reported unconfirmed.
- **Process-group programs.** Programs that call `setsid` or `setpgid` (for
  example `setsid`, job-control shells, or Python with `start_new_session`) get
  `EPERM`.
- **Windows concurrent spawns.** A `std::process` spawn on another thread can
  inherit the runner's pipe ends during `CreateProcessW`. The run then reports
  cleanup unconfirmed instead of hanging.
- **Process-group number reuse.** After the group empties, the final signal-0
  probe could reach a new group with the same number. This needs the kernel to
  reuse the number within one 10 ms poll, and the probe sends no signal.

## Next increments

| Increment | Scope | Must establish |
| --- | --- | --- |
| C2 | Linux Landlock filesystem rules and seccomp network denial | Read-only toolchain roots, writable workspace/scratch only, denied host data, denied IP sockets, a decision for Unix-domain sockets to host services, Landlock ABI detection that fails closed |
| C3 | Windows AppContainer | A per-workspace container identity, recorded and reversible access entries on the workspace and scratch, no network capability, denied host data |
| C4 | Trusted CLI `run` and the `execute` preference | Toolchain profiles, a curated environment, recorded profile with each result, journal records, and the acceptance cases in the development-tool contract |

Live qualification on each supported OS remains a separate gate.
