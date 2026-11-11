# Command execution

This guide owns the execution contract: how OMGCode starts a program, bounds
it, and stops its whole process tree, and which native confinement each platform
must apply before a command can be exposed. The
[development-tool contract](development-tools.md) owns toolchain profiles; the
[security rules](security-coding-rules.md#process-and-sandbox-boundary) own the
general process boundary.

## Status

Version 0.13.0 added the **process-tree runner core** in the
`omgcode-platform` crate (`process::run`). It applies no filesystem or network
confinement.

Version 0.14.0 added **`process::run_confined`**, which applies the
[Linux confinement](#linux-confinement) below: a Landlock filesystem allowlist
and a seccomp socket filter.

Version 0.15.0 adds [Windows confinement](#windows-confinement): the tree runs
in a per-workspace AppContainer with no capabilities, and trusted CLI commands
record and apply the [container grants](#container-grants) that decide what it
can reach.

No CLI command, model tool, or core API calls `run` or `run_confined`.
Settings reports keep `execution_allowed: false`. Do not describe this as
command support, and do not describe either confinement as qualified beyond
the tested kernel and Windows build.

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

## Confined runs

`process::run_confined(spec, confinement, limits, cancel)` takes the same
specification and limits as `run`, plus a `Confinement`:

| Field | Meaning |
| --- | --- |
| `read_only` | Paths that may be read, listed, and executed. Toolchain roots, system libraries, and the program belong here. |
| `read_write` | Paths that may also be written, created, renamed, truncated, and removed beneath. The workspace and a private scratch directory belong here. |
| `unix_sockets` | `PairsOnly` (default) or `HostServices`; see below. Windows refuses `HostServices`. |
| `container` | The workspace container name. Windows requires it; Linux ignores it. |

On Windows the paths are not applied at start: access comes from the
persistent [container grants](#container-grants), and the paths are checked
against them. See [Windows confinement](#windows-confinement).

Linux rules:

- Filesystem operations that Landlock handles are denied outside the listed
  paths, including in the user's home directory and other host data. Metadata
  changes and some other operations are not restricted anywhere; see
  [known limits](#known-limits). A directory rule covers everything beneath it.
  Rules are additive: a read-only path inside a read-write path is still
  writable.
- Each path must be absolute and name an existing directory, regular file, or
  character device. No component may be a symbolic link. At most 64 paths are
  accepted.
- IP sockets are refused in every mode, including loopback. There is no
  package-network or development-server networking yet.
- Setup fails closed. A missing path, a link, an unsupported type, a missing
  primitive, or an old kernel starts nothing. A program that the rules do not
  let the kernel execute fails as `Spawn` with `EACCES`.
- The outcome's `confinement` field records what applied:
  `Unconfined` for `run`, `Landlock { abi, unix_sockets }` on Linux, or
  `AppContainer` on Windows.

### Unix-domain sockets

The user chose a per-run mode with the stricter one as the default; see
[ADR-0028](adr/0028-confine-linux-runs-with-landlock-and-a-socket-filter.md).

| Mode | Allowed | Reaches host services |
| --- | --- | --- |
| `PairsOnly` (default) | `socketpair(AF_UNIX)` of stream or sequenced-packet type only | No |
| `HostServices` | `socket(AF_UNIX)` of any type, every Unix `socketpair`, and socket files in read-write paths | **Yes: every pathname socket the user can reach** |

`HostServices` exists for developers whose builds need the Docker daemon or a
similar service. Landlock cannot limit which pathname sockets are reached, so
it also exposes the D-Bus session bus, SSH agents, and any other socket the
user can open. Access to the Docker daemon is usually equivalent to root on the
host. A later caller must make this an explicit, recorded operator choice and
must never select it for a model.

On Windows, `HostServices` is refused with `Unsupported`. In `PairsOnly` mode a
host named pipe or Unix socket is reachable only when its own ACL admits the
container or every application package; a pipe with the default security
descriptor is not. The tree can still create pipes and sockets of its own.

## Container grants

A grant gives one workspace's container access to one directory and everything
beneath it. Grants are lasting ACL entries, recorded in the control journal.

| Command | Effect |
| --- | --- |
| `omgcode workspace container-grant <id> <path> --access read-write --revision <n>` | Grant the workspace directory itself. No other directory may be granted read-write. |
| `omgcode workspace container-grant <id> <path> --access read-execute --revision <n>` | Grant the workspace, a directory inside it, or a directory outside the development root, such as `%USERPROFILE%\.cargo`. |
| `omgcode workspace container-revoke <grant-id>` | Remove the entry and record it. Needs no revision. |
| `omgcode workspace status` | Lists outstanding grants as `pending`, `active`, or `revoking`. |

Rules:

- The container is `OMGCode.Workspace.<workspace id as 32 hex digits>`. Its SID
  is derived from that name, so it is stable for the workspace.
- No grant may overlap application storage. A read grant may not cover the
  development root or another workspace. One grant per workspace and directory;
  revoke it before changing its level. At most 32 grants per workspace and 256
  in all.
- The journal syncs `container_grant_requested` before the ACL changes; it
  needs the expected settings revision and advances it. After the change is
  read back, `container_grant_applied` marks the grant `active`. If the change
  fails, the grant stays `pending`, because the entry may exist; revoke it.
- Revocation records `container_revoke_requested`, removes the entry, confirms
  none remains, and records `container_grant_revoked`. If removal fails, the
  grant stays `revoking`; running the revocation again retries it. Revocation
  outcomes keep the revision.
- Every other journal append must leave room, in events and bytes, for each
  outstanding grant to reach a confirmed revocation, so a full journal never
  strands container access.
- The first grant creates the container's AppContainer profile. Revoking the
  container's last grant deletes the profile and its folder. If that deletion
  fails, the command reports an error after the revocation was recorded.
- The granted directory must still be the recorded one, by volume and file
  identity. A moved or replaced directory cannot be revoked through OMGCode
  until it is back at its recorded path.
- Grants work only on Windows. Elsewhere `container-grant` fails and records
  nothing.

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
  are gone. A member's process object can become signaled shortly after the
  count reaches zero, while the kernel finishes its exit, so a check for that
  object must allow a short wait.

### Windows confinement

For `run_confined`, the runner checks before anything starts:

1. `unix_sockets` is not `HostServices`; otherwise `Unsupported`.
2. `container` is set; otherwise `InvalidSpec`.
3. The container's profile exists; otherwise a `Confinement` error. The runner
   never creates a profile.
4. `LOCALAPPDATA` is in the environment; otherwise `InvalidSpec`. Windows needs
   it to start an AppContainer process. It replaces it with the container's
   folder, `%LOCALAPPDATA%\Packages\<name>\AC`, and sets `TEMP` and `TMP` to
   `AC\Temp`, which the container can write.
5. Each `read_only` path allows the container at least read-execute, and each
   `read_write` path read-write. Each path is opened without following a final
   reparse point. Allow entries for the container SID or for every application
   package count, inherited or explicit, less deny entries for them.
6. The program allows the container read-execute. `CreateProcessW` maps the
   image with the caller's token, so without this check a program the container
   cannot read would still start.

A failed check starts nothing. The runner then passes
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` with the container SID and no
capabilities, beside the job and handle lists above. Every descendant stays in
the same container and job.

Grants are applied by `container::grant`:

- Read-execute is `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`. Read-write adds
  `FILE_GENERIC_WRITE | DELETE`, without `WRITE_DAC`, `WRITE_OWNER`, or
  `FILE_DELETE_CHILD`. Both are inherited by directories and files.
- The directory is opened with backup semantics and without following a final
  reparse point. It must be an NTFS directory with the recorded identity.
- `SetEntriesInAclW` replaces any explicit entry for the SID, and
  `SetSecurityInfo` writes the DACL through the same handle, keeping its
  protected state. Windows propagates the entry to existing children; on a
  large tree this takes time, and the control journal stays locked meanwhile.
  Propagation does not follow junctions.
- The entry is read back and must be exactly one allow entry with the expected
  mask and inheritance. Revocation removes every explicit allow entry for the
  SID and confirms none remains.

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

### Linux confinement

For `run_confined`, the parent prepares everything that can fail before
`fork`:

1. It reads the Landlock ABI with `landlock_create_ruleset`. Without Landlock,
   or below ABI 3, it returns `Unsupported`. ABI 3 is the first that handles
   truncation, so earlier ABIs could not keep a read-only file intact.
2. It creates a ruleset that handles every filesystem right the ABI knows,
   TCP bind and connect from ABI 4, and signal and abstract-socket scoping from
   ABI 6. Rights added after ABI 6 are not handled.
3. It opens each path with `openat2(O_PATH, RESOLVE_NO_SYMLINKS |
   RESOLVE_NO_MAGICLINKS)`, checks its type, and adds a path-beneath rule.
   Read-only rules grant execute, read, and list. Read-write rules grant every
   handled right except creating device nodes, and except creating socket
   files in `PairsOnly` mode. File rules keep only the rights that apply to
   files.

Rules bind to the opened objects, so a later rename does not change the grant.
The ruleset descriptor is close-on-exec. The child then runs C1's steps and, after
`no_new_privs`, calls `landlock_restrict_self` before installing the seccomp
filter. The confined filter adds these refusals, each with `EPERM`:

- `socket` and `socketpair` for every family other than `AF_UNIX`, and the
  Unix rule for the selected mode;
- `io_uring_setup`, `io_uring_enter`, and `io_uring_register`, because
  `io_uring` can create sockets without `socket`.

Datagram socket pairs are refused in `PairsOnly` mode because `sendto` on a
connected Unix datagram socket can still address a host socket path, such as
`/dev/log`. Stream sockets refuse a destination address, and sequenced-packet
sockets ignore it.

## Known limits

These remain true after C3 and must be addressed or documented by the increments
that expose execution:

- **`run` is unconfined.** A program started by `run` can read and write
  anything the user can, use the network, and talk to host services. Only
  `run_confined` applies the rules above.
- **Windows grants are lasting and shared.** Grants are ACL entries that stay
  until revoked. A run's `Confinement` is checked against them, but grants it
  does not list still apply to it. Programs running as the same user can add or
  remove entries; the journal records only changes made through OMGCode.
- **Windows reads beyond grants.** Paths that every application package may
  read stay readable inside the container, including the Windows directory,
  `Program Files`, and many machine registry keys.
- **Windows hard links.** A hard link shares one security descriptor, so a
  grant reaches the file under every name, including names outside the granted
  directory. Grant directories that contain no hard links to host data.
- **Windows ancestors.** The container can open granted paths below
  directories it cannot read, but it cannot list or read those ancestors. Tools
  that search parent directories for configuration may fail; this was not
  tested.
- **Windows loopback.** Loopback traffic is dropped without a reply, so a
  connect fails only after the system timeout (about 21 seconds), not at once.
- **Windows build coverage.** Windows confinement was tested on Windows 11
  build 26300 only.
- **Linux kernel coverage.** Confinement needs Landlock ABI 3 (Linux 6.2 or
  later) with Landlock enabled. It was tested on WSL 6.6 (ABI 3) only.
- **What Landlock does not cover.** Landlock up to ABI 6 does not restrict
  `stat`, `readlink`, `chdir`, `access`, `flock`, or `fcntl`, or metadata
  changes with `chmod`, `chown`, `setxattr`, and `utime`. These work on any
  path the program can name, including unlisted host files, within the user's
  own rights. On ABI 3 and 4, device `ioctl` is not restricted either: a
  character device reachable through any rule, read-only included, accepts
  state-changing `ioctl` calls. Grant devices only when needed. Before ABI 6, a
  confined program can signal other processes of the same user.
- **`HostServices` exposure.** See [Unix-domain sockets](#unix-domain-sockets).
- **No IP networking.** Package downloads, loopback test servers, and
  development servers fail until a network profile is designed.
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
| C2 (0.14.0) | Linux Landlock filesystem rules and seccomp network denial | Done: see [confined runs](#confined-runs) and ADR-0028 |
| C3 (0.15.0) | Windows AppContainer | Done: see [Windows confinement](#windows-confinement), [container grants](#container-grants), and ADR-0029 |
| C4 | Trusted CLI `run` and the `execute` preference | Toolchain profiles, a curated environment, recorded profile with each result, journal records, an explicit recorded choice before `HostServices`, confinement built from the recorded grants on Windows, and the acceptance cases in the development-tool contract |

Live qualification on each supported OS remains a separate gate.
