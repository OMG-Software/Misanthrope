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

Version 0.15.0 added [Windows confinement](#windows-confinement): the tree runs
in a per-workspace AppContainer with no capabilities, and trusted CLI commands
record and apply the [container grants](#container-grants) that decide what it
can reach.

Version 0.16.0 adds [trusted runs](#trusted-runs). `omgcode run` starts one
program through `run_confined` under a recorded
[toolchain profile](#toolchain-profiles), behind the workspace `execute`
preference, and records it in the [run journal](#run-journal). It is a
trusted operator command. A model command tool can request one, but only a
trusted approval record lets it call the same runner; no model tool starts a
process without that record, and nothing calls the unconfined `run`. Settings
diagnostics keep `execution_allowed: false`, because a diagnostic never
authorizes a run. Do not describe either confinement as qualified beyond the
tested kernel and Windows build.

Version 0.17.0 adds [tree limits](#tree-limits): every trusted run is bounded
in memory, live processes, and CPU time for its whole tree. Windows applies
them to the run's Job Object. Linux applies them to a child of the runner's
delegated cgroup v2, and refuses a run when the runner is not in one.

Version 0.18.0 adds [network access](#network-access). A trusted run passes
`--network` to get outbound IP networking, and the workspace `task_network`
preference must allow it. Runs without the flag keep no IP networking.

Version 0.19.0 adds a sampled [disk-growth limit](#tree-limits). Every trusted
run bounds the allocated bytes it may add to its writable roots. No portable
per-run disk quota exists, so the supervisor measures growth on a separate
thread rather than the kernel enforcing a quota.

Version 0.23.0 adds a per-workspace package cache root. A configured root is
exposed read-only on Linux and Windows; Windows requires a read-execute
container grant on that root. Without a configured root, Linux keeps the
application-storage fallback and Windows exposes nothing. See
[ADR-0037](adr/0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md).

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
| `network` | `Denied` (default) or `Internet`; see [network access](#network-access). |
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
- With `network: Denied`, IP sockets are refused in every Unix-socket mode,
  including loopback. With `Internet`, see [network access](#network-access).
- Setup fails closed. A missing path, a link, an unsupported type, a missing
  primitive, or an old kernel starts nothing. A program that the rules do not
  let the kernel execute fails as `Spawn` with `EACCES`.
- The outcome's `confinement` field records what applied:
  `Unconfined` for `run`, `Landlock { abi, unix_sockets, network }` on Linux,
  or `AppContainer { network }` on Windows.

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
host. A [trusted run](#trusted-runs) selects it only when the workspace
`host_services` preference and the run's `--host-services` flag are both set,
and records the mode. No model can select it.

On Windows, `HostServices` is refused with `Unsupported`. In `PairsOnly` mode a
host named pipe or Unix socket is reachable only when its own ACL admits the
container or every application package; a pipe with the default security
descriptor is not. The tree can still create pipes and sockets of its own.

### Network access

The user chose the C6 options on 2026-11-13; see
[ADR-0032](adr/0032-allow-outbound-network-for-trusted-runs-through-task-network.md).

| Platform | `Denied` (default) | `Internet` |
| --- | --- | --- |
| Linux | No `AF_INET` or `AF_INET6` socket. | `socket(AF_INET)` and `socket(AF_INET6)` of any type. **Every destination is reachable, including loopback services, the local network, and cloud metadata endpoints.** On Landlock ABI 4 and later, an explicit TCP bind is denied, but `listen` on an unbound socket still accepts connections on a port the kernel chooses. |
| Windows | The container has no capabilities: no network, including loopback. | The container gets the `internetClient` capability: outbound connections to the internet. Loopback, private networks, and inbound connections stay denied. |

On both platforms a networked tree can send any data it can read, workspace
files included, to any host it can reach. Every other socket family keeps its
rule, `socketpair` stays Unix-only, and `io_uring` stays denied.

On Linux, name resolution and certificate checks read host files, which the
rules would otherwise hide. `process::network_support_paths()` returns the
existing entries of this fixed list, each resolved to its real path, and a
trusted run adds them read-only:

- `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/gai.conf`,
  `/etc/host.conf`, `/etc/services`, and `/etc/protocols`;
- `/etc/ssl/certs`, `/etc/ssl/cert.pem`, `/etc/ssl/openssl.cnf`,
  `/etc/pki/tls/certs`, `/etc/pki/tls/cert.pem`, `/etc/pki/tls/openssl.cnf`,
  `/etc/pki/ca-trust/extracted`, `/etc/ca-certificates/extracted`,
  `/usr/share/ca-certificates`, and `/usr/local/share/ca-certificates`.

The list names public trust stores, not `/etc/ssl` or `/etc/pki`, which hold
private keys beneath them. A path whose real path is, lies beneath, or
contains `/etc/ssl/private` or `/etc/pki/tls/private` is left out. A trust
store in another place, or a private key kept inside a trust store, is not
covered: list such a store as a profile root only after checking it.

A link such as `/etc/resolv.conf` is resolved when the run starts; a later
change of its target is not followed. Resolvers that need other services, such
as `nscd` or `systemd-resolved` through a Unix socket, need host services too.
Windows resolves names and checks certificates through system services, so it
adds no paths.

### Package-network profile (contract only)

The user selected a package-network profile as the next increment.
[ADR-0034](adr/0034-gate-package-networking-behind-enforceable-destination-control.md)
defines its contract, and version 0.20.0 records operator-declared profiles as
configuration. No run accepts a profile, the runtime has no destination
allowlist, and `--network` remains all-or-nothing.

A profile is an operator-declared, revisioned list of registry origins:
scheme, host, optional port, and an optional path prefix. Normal use is
`https`; `http` is allowed only for a loopback test fixture. A profile has no
credentials, wildcards, user information, or redirects to another origin. It
is configuration only and never authorizes a run. At most 32 profiles are
recorded, each with 1 to 8 origins.

`workspace package-profile-add <name> --registry <origin>... --revision <n>`,
`package-profile-show <name>`, and
`package-profile-remove <name> --revision <n>` manage the records. Each
mutation needs the expected settings revision and increments it.

Any implementation must pass these gates before it can be described as
implemented:

- the confined tree cannot reach any destination other than the declared
  origins or a trusted proxy, including direct IP, IPv6, UDP, DNS, loopback,
  alternate socket families, and paths that bypass the proxy;
- the mechanism fails closed, and a platform that cannot enforce it refuses
  the profile rather than falling back to general network access;
- its exact limits and bypass analysis are documented and qualified on both
  supported hosts;
- registry credentials remain a separate reviewed design and never enter a
  profile, run environment, journal, or model context;
- acceptance tests prove allowed-origin success, undeclared-origin refusal,
  redirect refusal, credential refusal, offline fallback, and platform refusal.

The current primitives cannot meet that gate. Landlock ABI 4 and later filters
TCP bind and connect by port, not address. Classic seccomp cannot read the
`sockaddr` contents. Windows AppContainer capabilities cannot name
destinations, and granting none blocks loopback too. Proxy environment
variables and an advisory loopback proxy can be bypassed by a hostile tree.
Candidate mechanisms for a later decision are a trusted host-side fetch into a
private cache with an offline run, an OS destination filter, or a Linux-only
network-namespace relay.

### Trusted package fetch

Version 0.21.0 adds a trusted operator fetch for declared origins; see
[ADR-0035](adr/0035-fetch-packages-through-a-trusted-allowlisted-cache.md).
It is never a model tool, and the confined run still has no network access.

```text
omgcode packages fetch <workspace-id> --profile <name> --url <url>
    --name <cache-file> [--max-bytes <1-1073741824>]
    [--timeout-secs <1-600>]
omgcode workspace set-package-cache <workspace-id> <dir> --revision <n>
omgcode workspace clear-package-cache <workspace-id> --revision <n>
```

`set-package-cache` selects one workspace's cache directory. It must exist,
must not overlap application storage or the development root, and must not
overlap another workspace's root. `clear-package-cache` removes the selection
and leaves cached files in place. Without a selected root, Linux uses the
application-storage fallback and Windows has none. A 0.23.0 control journal
cannot be read by 0.22.0.

Before the request it checks:

1. The workspace is registered and its live `task_network` preference is
   enabled.
2. The named package-network profile exists.
3. The URL is absolute and matches a declared origin's scheme, host, port, and
   path prefix. It has no credentials, query, or fragment. Normal use is
   `https`; `http` is accepted only for a loopback test fixture.
4. The cache name is one safe file name and the file does not already exist.
5. The byte limit is 1 byte to 1 GiB and the deadline is 1 to 600 seconds.

The request is one GET. Redirects are not followed, proxy environment
variables are ignored, no credentials are sent, and nothing is retried. TLS
uses the operating-system trust store through `reqwest`'s `native-tls`
feature; certificate validation is never disabled. The response must be 2xx
and its body is streamed, hashed with SHA-256, and stopped if it passes the
byte limit.

The file is written as a private `.partial-<fetch-id>` entry, synced, and
renamed into the selected workspace cache root, or into
`<data-dir>/package-cache/<workspace-id>/` when no root is selected. Existing
names are never overwritten. The cache makes no power-loss claim. The control
journal records `package_fetch_started` before the request and
`package_fetch_finished` after it; both preserve the settings revision. They
record the profile identity and revision, a SHA-256 of the URL (never its
text), the cache name, limits, HTTP status, byte count, and content digest. A
failure records a controlled reason. An abandoned fetch leaves its start
record without a finish; nothing retries automatically.

The run does not contact the registry itself. When the workspace cache exists
as a plain directory, the run adds only that directory to its read-only rules
and sets `OMGCODE_PACKAGE_CACHE` to it. The parent, application storage,
journals, settings, and other workspaces' roots stay outside the run's view,
and the run cannot write the cache. On Windows the selected root requires a
read-execute container grant on that directory; without it the run fails
before the program starts. Without a selected root, Linux uses the
application-storage fallback and Windows exposes nothing. The root's ancestors
must be traversable by the container, so use a dedicated top-level directory
rather than a user's private temporary tree. Tool-specific use of the
directory is operator configuration. Registry credentials remain unsupported.
`workspace status` lists selected roots under `package_cache_roots`.

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
- A per-workspace package cache root outside the development root and
  application storage is a valid read-execute grant target. Grant that root,
  not a shared parent. The grant is container-wide, so use separate roots for
  workspaces that must stay isolated. The container must be able to traverse
  the root's ancestors; a dedicated top-level directory is recommended.
- Grants work only on Windows. Elsewhere `container-grant` fails and records
  nothing.

## Trusted runs

The user chose the C4 options on 2026-11-11; see
[ADR-0030](adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md).

```text
omgcode run <workspace-id> --profile <name> [--cwd <relative dir>]
    [--timeout-secs <1-3600, default 600>]
    [--max-output-bytes <at most 16 MiB, default 1 MiB>]
    [--max-memory-mib <16-1048576, default 8192>]
    [--max-processes <1-65536, default 1024>]
    [--max-cpu-secs <1-604800, default 7200>]
    [--max-disk-mib <16-1048576, default 8192>]
    [--host-services] [--network] -- <program> [arguments...]
omgcode runs audit [--limit <n>] [--archive <file>]
omgcode runs acknowledge <run-id>
omgcode runs archive --archive-dir <existing dir>
```

`run` prints `{"run": report}`. The report holds:

- the recorded `start` and `finish`;
- `stdout` and `stderr`, each as text with `utf8`, `total_bytes`, and
  `truncated`;
- on Linux, `scratch_removed`;
- for a `revoked` run, `revoked_because`.

`finish.usage` gives the tree's CPU time and, where the platform reports it,
peak memory. Since 0.19.0 it also gives `peak_disk_bytes`, the highest net
growth of the writable roots above the pre-run baseline.

The command succeeds whenever the run was supervised and its outcome recorded,
whatever the program's exit code. Read `run.finish.result`. A failure prints a
JSON error with code `run`.

### Checks

While holding the control journal lock, `run` checks:

1. The workspace is registered, and its live `execute`, `read`, and `write`
   preferences are enabled. A run writes inside the workspace, so it needs all
   three.
2. For `--host-services`: the host is Linux, and the live `host_services`
   preference is enabled. For `--network`: the live `task_network`
   preference is enabled.
3. The profile exists and was declared on this platform. Each root and `PATH`
   entry is still the recorded directory, and no root now overlaps the
   development root or application storage. Otherwise the profile is stale:
   remove it and add it again.
4. The working directory is a relative path that stays inside the workspace.
   The default is the workspace itself.
5. The program resolves:
   - A bare name is searched in the profile's `PATH` entries, in order. On
     Windows `.exe` is tried first. The caller's `PATH` is never searched.
   - A relative path containing a separator names a file in the workspace.
   - An absolute path is used as given.

   The file must exist, and its directory must lie inside a profile root or
   the workspace. The path that runs is the one found, before links resolve.
6. On Windows, the workspace has an active read-write container grant, and no
   grant of the workspace is `pending` or `revoking`.
7. The [tree limits](#tree-limits) are in range and can be applied. On Linux
   the CLI must run in a delegated cgroup.

A refused run records nothing. Then the run takes its owner lock, the intent
is synced to the run journal, the control and journal locks are released, and
the program runs.

### Settings changes during a run

A run's watcher rereads the settings each time the control journal changes,
and checks the run against them:

- the workspace is still registered and inside the development root;
- `execute`, `read`, and `write` are still enabled, `host_services` for a
  run that selected host services, and `task_network` for a run that
  selected network access;
- the run's profile still exists with the same identity;
- on Windows, the workspace still has its active read-write grant, no grant of
  the workspace is `revoking`, and no revocation of any grant of the workspace
  was requested since the run started. Every grant applies to the container,
  so revoking any of them withdraws the run, including a grant added after it
  started. The watcher compares a per-workspace count of revocation requests,
  which it cannot miss between two polls. A `pending` grant only adds access,
  so it does not stop a run.

When a check fails, or the settings cannot be read, the watcher stops the whole
tree. The outcome is recorded as `revoked`, and the report gives the reason in
`revoked_because`.

`workspace permissions`, `profile-remove`, `container-revoke`, and `set-root`
record their change first. Then they release the control lock and wait up to
60 seconds for every pending run that the new settings no longer allow.
`container-revoke` also waits for every pending run of the grant's workspace
that started before the revocation; it lists them while it holds the control
lock, under which every run starts. If the run journal cannot be read, for
example after an interrupted append, it still revokes the grant, then fails
with code `run` and says that the grant was revoked.

A run counts as stopped only when its owner lock is free and an outcome other
than `cleanup_unconfirmed` is recorded. These runs are reported as not stopped,
and the command fails with code `run` and lists them:

- a run still live after the wait;
- a run whose cleanup was unconfirmed, because processes may remain, until it
  is acknowledged;
- a run whose supervisor ended without an outcome, even if it was acknowledged
  while the command waited.

The change itself stays recorded. Inspect the runs with `runs audit`. A change
that withdraws nothing returns at once.

Settings commands wait up to 10 seconds for another command's control lock,
because run watchers hold it briefly while they reread the settings.

### Environment and confinement

Nothing is inherited. The run receives `PATH` joined from the profile's
entries, the runner-owned variables below, and the profile's variables.

| Platform | Runner-owned variables | Read-only | Read-write |
| --- | --- | --- | --- |
| Linux | `HOME`, `TMPDIR`, and `XDG_CACHE_HOME`, all set to `<data-dir>/run-scratch/<run id>` (mode 0700, removed after the run), and, when the workspace cache exists, `OMGCODE_PACKAGE_CACHE` set to the selected root or the application-storage fallback | Profile roots, `/dev/zero`, `/dev/random`, `/dev/urandom`, the workspace's [package cache](#trusted-package-fetch) when it exists, and, with `--network`, the [network support paths](#network-access) | The workspace, the scratch directory, `/dev/null` |
| Windows | `LOCALAPPDATA`, `SystemRoot`, `windir`, and `HOME` and `USERPROFILE` set to the container's `AC` folder. Windows then sets `TEMP` and `TMP` inside it. When a granted package cache root exists, `OMGCODE_PACKAGE_CACHE` is set to it | Profile roots, checked against their live ACLs; the package cache root, checked against its grant | The workspace, through its grant |

Linux runs use `PairsOnly` sockets unless host services were selected. Runs
have no IP networking unless `--network` was passed. Windows
roots that every application package can read, such as `Program Files`, need
no grant; others need a read-execute [container grant](#container-grants).

`SIGINT` and `SIGTERM` on Linux, and Ctrl+C and Ctrl+Break on Windows, cancel
the run. The tree is terminated and the outcome is recorded as `cancelled`.
The interrupt watcher belongs to the run and is stopped and joined when it
ends.

### Tree limits

The user chose the C5 options on 2026-11-12 and the C7 option on 2026-11-13;
see [ADR-0031](adr/0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md)
and [ADR-0033](adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).
Every trusted run has all four limits. There is no unlimited run.

| Limit | Flag and default | Windows | Linux |
| --- | --- | --- | --- |
| Memory | `--max-memory-mib`, 8192 | `JobMemoryLimit`: committed private memory of the whole job | `memory.max`, with `memory.swap.max` 0 where swap is accounted and `memory.oom.group` 1 |
| Processes | `--max-processes`, 1024 | `ActiveProcessLimit`: processes alive at once | `pids.max`: tasks alive at once, so threads count |
| CPU time | `--max-cpu-secs`, 7200 | Job accounting: user plus kernel time of every process the job held | `cpu.stat` `usage_usec` of the run's cgroup |
| Disk growth | `--max-disk-mib`, 8192 | Sampled allocated bytes under the workspace and the container profile folder | Sampled allocated bytes under the workspace and the per-run scratch directory |

The kernel enforces memory and processes. The supervisor enforces CPU time by
polling and disk growth by sampling. When the tree reaches a limit, the runner
terminates the whole tree and records `limit_exceeded` with the limit, even if
the program then exited by itself:

- **Memory:** on Linux the kernel kills the whole cgroup when it runs out of
  memory, and the runner sees `oom_kill` in `memory.events`. On Windows an
  allocation that would pass the limit fails, and the job posts a memory-limit
  message.
- **Processes:** a start that would pass the limit fails. Linux counts it in
  `pids.events`; Windows posts an active-process-limit message.
- **CPU time:** the supervisor compares the tree's accounting with the limit
  at each 10 ms poll. A tree with many busy threads can pass it by up to one
  poll's worth of CPU time per thread before it is stopped.
- **Disk growth:** the supervisor measures the writable roots before the
  program starts and once a second while it runs, on a separate thread. Either
  the allocated-byte total or the file-length total passing its baseline plus
  the limit ends the tree. A final measurement runs after the tree is empty,
  so a short run that wrote past the limit and exited is still recorded as
  `limit_exceeded`. See [disk growth](#disk-growth).

The report and the `run_finished` record give the tree's CPU time and, where
the platform reports it, its peak memory: the job's peak commit on Windows, or
`memory.peak` on Linux 5.19 or later. They also give the highest net disk
growth observed above the pre-run baseline.

Windows does not guarantee delivery of job messages. If one is lost, the job
still enforces the limit, but the run ends as the program decides and is not
recorded as `limit_exceeded`. CPU time and disk growth do not depend on
messages.

Windows also sends the limit messages of a job nested in the run's job, such
as a tool's own stricter job, to the run's port. So the runner counts a
message only when the run's own job confirms it:

- **Processes:** the job holds as many processes as its limit.
- **Memory:** the job's peak commit came within a sixteenth of its limit. An
  allocation refused further below the limit, because it was large, is still
  refused but is not recorded as `limit_exceeded`.

These checks narrow attribution but do not identify the sending job. A job
message carries the receiving port's own completion key and, for memory, only
the process ID. If the run is already within a sixteenth of its memory limit
when a nested job reaches its own memory limit, the runner still ends the run
as `limit_exceeded` for memory.

Each 10 ms poll takes at most 256 job messages, so a tree that keeps starting
processes cannot hold the supervisor away from its other checks. Once the
program has exited, the runner takes up to 64 such batches before it stops the
tree, until the queue is empty or a limit is confirmed. So a limit reported
behind other messages still decides the result. If messages remain after those
batches and CPU time is within its limit, the runner cannot tell whether a
limit was reached. It still stops the tree, then records `supervision_failed`
rather than a normal exit. On Windows 11 build 26300, a
nested job's limit messages did not reach the run's port in testing; the
check follows the documented behaviour.

#### Disk growth

No portable per-run disk quota exists for an unprivileged runner: Job Objects
have no disk accounting, a cgroup v2 has no disk controller, and filesystem
quotas need administrative setup. The limit is therefore sampled by the
supervisor; it is not a kernel quota. See
[ADR-0033](adr/0033-bound-run-disk-growth-by-sampling-writable-roots.md).

- **Measured roots:** the confinement's read-write directories and regular
  files, except character devices such as `/dev/null`. On Windows the
  container profile folder (`%LOCALAPPDATA%\Packages\<name>\AC`) is also
  measured, because the container writes there without a grant. A root inside
  another root counts once.
- **Baseline and growth:** the allocated-byte and file-length totals are
  measured immediately before the program starts. Each budget is that baseline
  plus the limit. Deletions reduce the totals, so the recorded
  `peak_disk_bytes` is net growth.
- **Allocation and length:** Linux uses `st_blocks * 512` for allocation,
  Windows uses the allocated (compressed) size, and both use the file length.
  Each file contributes to both totals, and the limit applies to each. Length
  catches writes that a filesystem has not written back yet; allocation
  catches a pre-existing sparse file being filled in. A sparse file is
  therefore counted conservatively at both its length and its allocation.
- **Sampling:** a dedicated thread measures every second. The supervisor
  checks its result at the normal 10 ms poll, and a final measurement runs
  after the tree is empty. A tree can pass the limit between samples; the
  overshoot depends on the write rate.
- **Fail closed:** a baseline that cannot be measured records `not_started`.
  An unreadable root or more than 4,000,000 entries while the run lives stops
  it as `supervision_failed` rather than claiming a limit that was not
  measured. Symlinks and other reparse points are not followed. A run that
  makes a writable path unreadable to the supervisor is stopped for the same
  reason.
- **Not counted:** directory entries, inode or MFT use, and filesystem
  metadata. A hard-linked file is counted once per name, so several names
  overcount it. Other writers to the same roots are counted as the run's
  growth. A tree that deletes and rewrites the same bytes can churn more than
  the limit without adding total use.

#### Linux delegated cgroup

Linux needs the runner itself in a cgroup v2 that is delegated to its user.
Start the CLI in a scope of its own:

```text
systemd-run --user --scope -p Delegate=yes -- omgcode run <workspace-id> ...
```

In scope mode `systemd-run` replaces itself with `omgcode`, so the standard
streams, exit status, and signals are the CLI's own. Before recording a run,
the CLI checks:

- `/proc/self/cgroup` names one unified v2 cgroup, and `/sys/fs/cgroup` is the
  v2 hierarchy;
- that cgroup, its `cgroup.procs`, and its `cgroup.subtree_control` belong to
  the current user, and it is not the root;
- it offers the `memory` and `pids` controllers and has `cgroup.kill`
  (Linux 5.14 or later).

Then the CLI moves itself into a leaf, `omgcode-supervisor`, and enables
`memory` and `pids` for the delegated cgroup's children. cgroup v2 allows that
only while the delegated cgroup holds no process, so a scope shared with a
shell or with `cargo` is refused. Each run gets `omgcode-run-<pid>-<n>` beside
the supervisor leaf. The program joins it before `exec`, before any other
setup, so every descendant is counted. The runner kills the tree with both the
process group and `cgroup.kill`, and counts it empty only when the group is
gone and `cgroup.events` reports `populated 0`. It removes the run cgroup
afterwards; a cgroup that cannot be removed makes cleanup unconfirmed.

The confined tree's Landlock rules let it write only to the workspace, its
scratch directory, and `/dev/null`, so it cannot change its limits or leave
its cgroup. Do not place a development root under `/sys/fs/cgroup`. Other
programs of the same user, outside the run, can still change the delegated
cgroup.

### Model command tool

Version 0.25.0 adds `workspace_run_command` to the core turn runtime. The
runtime advertises it only when the session workspace's live `model_tools`,
`execute`, `read`, and `write` preferences are enabled. Each call rechecks
them, and the trusted run checks them again.

Arguments are a recorded toolchain `profile`, a `program`, at most 64
`arguments`, and an optional workspace-relative `cwd`. There is no shell.
The tool fixes its limits: a 20-second deadline, 4 KiB retained from each
output stream, 1 GiB memory, 64 processes, 20 seconds of CPU, and 256 MiB of
disk growth. It selects no network or host services.

The first call matches no approval, records a pending approval with a bounded
summary that contains no argument text, and returns `approval_required` with
the approval identifier. Nothing runs. The operator runs
`approvals approve <id>`; the model must call again with the same arguments.
The runtime checks the exact digest, session, workspace, operation, and
settings revision, consumes the approval once, drops the settings lock, and
calls the trusted `run` path. A denial returns `approval_denied` and is not
re-requested. An expired or consumed record causes a new pending request. A
run that fails after consumption uses the approval.

Output is untrusted and bounded. The session journal records the tool intent
and result; the run journal records the run. No replay executes a recorded
call. The model can request but cannot approve; the CLI approval commands are
the operator surface, and there is no approval UI. A CLI acceptance test runs
one approved command through the real chat process, a mock provider, and the
trusted runner on both Linux and Windows, and asserts a successful exit, the
bounded output, and the consumed approval. On Linux the chat process starts in
a delegated cgroup; on Windows the test scene grants the workspace.

### Toolchain profiles

Profiles are declared by the operator. Nothing is discovered automatically.

| Command | Effect |
| --- | --- |
| `omgcode workspace profile-add <name> --root <dir>... [--path <dir>]... [--env NAME=VALUE]... --revision <n>` | Record a profile. Needs the expected revision and increments it. |
| `omgcode workspace profile-show <name>` | Show one profile. |
| `omgcode workspace profile-remove <name> --revision <n>` | Remove it. Needs the expected revision and increments it. |
| `omgcode workspace status` | Lists profiles under `profiles` with the revision that added each. |

Rules:

- **Names:** at most 64 ASCII letters, digits, `.`, `-`, or `_`, starting with
  a letter or digit. Names are unique case-insensitively. Each profile records
  the platform it was declared on.
- **Roots:** 1 to 16 existing directories, recorded by identity. They may not
  overlap the development root in either direction, overlap application
  storage, or duplicate each other.
- **`PATH` entries:** up to 16, each an existing directory inside a root.
- **Variables:** up to 64, with names of at most 128 bytes and values of at
  most 4096 bytes, and no NUL.
  - A name starts with a letter or `_`, continues with letters, digits, or
    `_`, and is unique case-insensitively.
  - `PATH`, `HOME`, `TMPDIR`, `TEMP`, `TMP`, `USERPROFILE`, `LOCALAPPDATA`,
    `SYSTEMROOT`, `WINDIR`, `XDG_CACHE_HOME`, and `OMGCODE_PACKAGE_CACHE` are
    reserved for the runner, in any case.
  - Values are stored in the private settings journal and shown by `status`.
    Never put secrets in them.
- **Limit:** at most 32 profiles.
- **Changes:** there is no edit. Remove a profile and add it again. The
  journal events are `profile_added` and `profile_removed`.

A profile lists what a toolchain needs, not just its executable. Tools that
read configuration outside their installation need that directory as a root.
For example, a distribution Node reads `/etc/ssl`.

### Run journal

`<data-dir>/runs-v1.jsonl` sits beside the control journal and shares its
store identity. Each line has schema version 1, an event UUID, a contiguous
sequence, a UTC timestamp, and a tagged `action`:

| Action | Meaning |
| --- | --- |
| `created` | Header |
| `run_started` | Synced before the program starts. Records the run ID, workspace, settings revision, profile ID, name, and revision, program path, argument count, `arguments_sha256`, working directory, socket mode, since 0.17.0 `limits` (`max_memory_bytes`, `max_processes`, `max_cpu_ms`, and since 0.19.0 `max_disk_bytes`), since 0.18.0 and only for a networked run `network` with the `support_paths` it could read (at most 32), and, since 0.22.0 and only when true, `package_cache` for read access to the workspace cache. |
| `run_finished` | Records the result (`exited`, `signalled`, `deadline_exceeded`, `cancelled`, `revoked`, `limit_exceeded` with its `limit`, `not_started`, `supervision_failed`, or `cleanup_unconfirmed`), the applied confinement, the elapsed time, the output byte counts, and, for a started run, `usage` (`cpu_ms`, where reported `peak_memory_bytes`, and since 0.19.0 `peak_disk_bytes`) |
| `run_acknowledged` | The operator cleared a pending run |

How it works:

- **What is not recorded:** argument text, environment values, and output.
  The digest is SHA-256 over each argument's native bytes, each prefixed by
  its length as a little-endian `u64`.
- **Pending runs:** a run whose outcome is missing, or whose cleanup was
  unconfirmed, stays pending.
  - `runs audit` lists pending runs.
  - `runs acknowledge` clears one. That claims no outcome and retries
    nothing. On Linux it also removes a leftover scratch directory.
  - A run whose supervising process is still live cannot be acknowledged.
    That process holds `<data-dir>/run-owners/<run id>.lock` from before the
    intent is recorded until the outcome is recorded.
- **Limits:** at most 16 pending runs; 8 MiB, 4,096 events, and 64 KiB per
  event. A journal that records the 0.19.0 fields cannot be read by 0.18.0.
  A 0.22.0 journal that records `package_cache` cannot be read by 0.21.0.
- **Reserve:** a new run must leave room for every pending run's remaining
  events, 1 KiB each: two for a run in progress, one for a run that needs
  acknowledgement.
- **Archiving:** `runs archive` moves a journal with no pending runs into an
  existing directory and reports its SHA-256. A new journal starts on the next
  run. `runs audit --archive` reads an archived file. A bare file name names
  a file in the current directory.
- **Locking:** the journal lock is held only while appending. A busy lock is
  retried for up to 10 seconds.

A run whose outcome cannot be recorded still prints its report, then fails;
it stays pending.

### Acceptance

`crates/omgcode-cli/tests/run_workflow.rs` covers these cases with system
tools on both platforms:

- confinement and recording;
- refusals;
- host-service selection;
- stale profiles;
- deadlines, concurrency, and abandoned runs;
- every tree limit, including disk growth;
- interrupts, on Linux.

`crates/omgcode-cli/tests/toolchain_acceptance.rs` runs real Rust, Python,
Node, C/C++, Go, and Java workflows. Its file header lists the toolchain
variables. Each workflow:

- builds or compiles, runs tests, formats where the toolchain has a formatter,
  and runs scripts;
- runs a subordinate process;
- keeps its outputs in the workspace;
- fails a missing dependency as an ordinary exit;
- runs a hostile program that must be denied host data, writes outside the
  workspace, and a loopback listener.

The C program checks files only. These tests are ignored by default, and a
missing toolchain variable fails the test.

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
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` with the container SID, beside
the job and handle lists above. Without network access it lists no
capabilities. With `Internet` it lists only the `internetClient` capability
SID, enabled. Every descendant stays in
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
   explicit TCP bind from ABI 4, TCP connect from ABI 4 unless the run has
   network access, and signal and abstract-socket scoping from ABI 6. Rights added
   after ABI 6 are not handled. No network rule is added, so a handled right
   is denied for every port.
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

- `socket` for every family other than `AF_UNIX`, except `AF_INET` and
  `AF_INET6` with network access, and the Unix rule for the selected mode;
- `socketpair` for every family other than `AF_UNIX`;
- `io_uring_setup`, `io_uring_enter`, and `io_uring_register`, because
  `io_uring` can create sockets without `socket`.

Datagram socket pairs are refused in `PairsOnly` mode because `sendto` on a
connected Unix datagram socket can still address a host socket path, such as
`/dev/log`. Stream sockets refuse a destination address, and sequenced-packet
sockets ignore it.

## Known limits

These remain true after C6:

- **`process::run` is unconfined.** A program started by the platform
  function `run` can read and write anything the user can, use the network,
  and talk to host services. Only `run_confined` applies the rules above.
  The trusted CLI `run` uses `run_confined` only.
- **Profile variables are not secret.** They are stored in the settings
  journal and shown by `status`. There is no credential path for runs.
- **Node on Windows.**
  - Node's module loader reads every ancestor directory, starting at the drive
    root, and fails with `EPERM` in a container. Declare
    `NODE_OPTIONS=--preserve-symlinks --preserve-symlinks-main` in the
    profile.
  - Child processes with piped standard streams never return, even with a
    timeout; only the run deadline ends them. Inherited streams work. This
    includes the isolated `node --test`; use `--test-isolation=none` (Node
    22.8 or later).
- **Acceptance coverage.** These results are from 0.16.0, before tree
  limits. The toolchain cases have not been rerun under the 0.17.0 limits.
  They passed with:
  - Windows: Rust 1.99.1 (MSVC 14.50 and SDK 10.0.26100), Python 3.14.4,
    Node 24.15.0, Go 1.27.4, and MSVC C/C++;
  - Linux: Rust 1.99.1, Python 3.12.3, Node 18.19.1, Go 1.26.10, and
    GCC 13.3.

  The Java case has not run, because neither host had a JDK. Not covered:
  disk exhaustion; package downloads through `--network`, which have not
  been run as acceptance cases; and Python and Node formatters, because none
  were installed. On
  Windows, the Rust, Python, and Node toolchains were user-owned copies
  granted read-execute, because the installed directories needed
  administrator rights to grant.
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
  that search parent directories or resolve real paths may fail; Node does, as
  described above.
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
- **Network access is all or nothing.** Without `--network`, package
  downloads, loopback test servers, and development servers fail. With it,
  a Linux run reaches every destination, loopback and the local network
  included, and a Windows run reaches the internet but not loopback. There is
  no destination allowlist. A networked Linux run can accept TCP
  connections: on ABI 3 through any bind, and on ABI 4 and later through
  `listen` on an unbound socket, which takes a port the kernel chooses. UDP
  bind is never limited. See
  [network access](#network-access).
- **No run destination enforcement.** Package-network profiles are recorded,
  and the trusted [`packages fetch`](#trusted-package-fetch) command contacts
  only declared origins. No run accepts a profile, and `--network` remains
  all-or-nothing. On Linux a run can read only its workspace's cache; Windows
  does not expose the cache. A proxy or proxy environment variables are not a
  boundary: a hostile tree can connect directly.
- **Package fetch limits.** The fetch is trusted-CLI only. It sends no
  credentials, follows no redirects, ignores proxy variables, and uses the
  system TLS store. The cache is the selected per-workspace root or, on Linux,
  the application-storage fallback. It makes no power-loss claim. Tests use
  loopback HTTP; a live HTTPS registry is not qualified.
- **Package cache exposure.** A run receives `OMGCODE_PACKAGE_CACHE` when the
  workspace cache exists as a plain directory, and may read only that
  directory. On Windows the selected root needs a read-execute container grant
  and traversable ancestors, or the run fails before it starts. Linux falls
  back to application storage when no root is selected; Windows exposes
  nothing. There is no invalidation, eviction, or freshness check, and
  configuring a toolchain to use the directory is operator work.
- **Model command limits.** The model command tool accepts no model-selected
  limits, network, or host services. It uses a 20-second deadline and 4 KiB
  per output stream. The operator flow spans two turns, and no approval UI
  exists. A denied exact request stays denied for the current settings
  revision; a failed run consumes the approval. A CLI acceptance test runs one
  approved command through the real chat process and the trusted runner on
  both hosts; it needs a delegated cgroup on Linux and a workspace container
  grant on Windows. Confinement correctness remains the platform suites'
  responsibility.
- **Network coverage.** Network access was tested on Windows 11 build 26300
  and WSL 6.6 (Landlock ABI 3) only, including a TLS fetch with the system
  `curl` on both. The ABI 4 TCP-bind denial has not run on a live kernel.
- **Tree limits differ by platform.** Windows limits committed private memory;
  Linux limits cgroup memory, which includes page cache and kernel memory
  charged to the tree. Linux counts threads against the process limit. CPU
  time is checked by polling and can overshoot; see [tree limits](#tree-limits).
- **Disk growth is sampled, not a quota.** The supervisor measures the
  writable roots once a second, so a tree can pass the limit between samples.
  Growth is net and applies to allocated bytes and file lengths separately, so
  a write is seen before writeback and a sparse file is counted
  conservatively. Directory entries, inode or MFT use, and filesystem metadata
  are not counted; a hard-linked file is counted once per name. Other writers
  are counted as the run's growth. A run that
  makes a writable path unreadable to the supervisor, or whose roots hold more
  than 4,000,000 entries, fails as `supervision_failed`. See
  [disk growth](#disk-growth).
- **Disk-limit coverage.** The limit was tested on Windows 11 build 26300 and
  WSL 6.6 only. The tests write 16 MiB or more and observe `limit_exceeded`;
  they do not establish behaviour under a full volume, concurrent writers, or
  inode exhaustion.
- **Linux needs a user service manager.** Every Linux run needs a delegated
  cgroup, normally a `systemd-run --user` scope. Hosts without a user systemd
  instance, or with cgroup v1 or a hybrid hierarchy, cannot run. Tree limits
  were tested on WSL 6.6 only.
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
| C4 (0.16.0) | Trusted CLI `run` and the `execute` preference | Done: see [trusted runs](#trusted-runs) and ADR-0030 |
| C5 (0.17.0) | Tree-wide memory, process, and CPU-time limits | Done: see [tree limits](#tree-limits) and ADR-0031 |
| C6 (0.18.0) | Outbound network access behind `task_network` and `--network` | Done: see [network access](#network-access) and ADR-0032 |
| C7 (0.19.0) | Sampled disk-growth limit for every trusted run | Done: see [disk growth](#disk-growth) and ADR-0033 |
| C8 (contract only, 0.19.0) | Package-network profile contract and implementation gate | Done as a contract: see [package-network profile](#package-network-profile-contract-only) and ADR-0034. No destination enforcement. |
| C9 (0.20.0) | Package-network profile records | Done as configuration: `workspace package-profile-add/show/remove` and the control journal events. No run accepts a profile. |
| C10 (0.21.0) | Trusted allowlisted package fetch | Done: see [trusted package fetch](#trusted-package-fetch) and ADR-0035. The run remains offline. |
| C11 (0.22.0) | Read-only package cache for Linux runs | Superseded by C12: Linux exposure moved to the per-workspace root, with an application-storage fallback. |
| C12 (0.23.0) | Per-workspace package cache root and scoped Windows grant | Done: see [trusted package fetch](#trusted-package-fetch) and ADR-0037. Windows needs a read-execute grant on the workspace root. |
| C13 (0.24.0) | Provider profiles and trusted approval records | Done: see the [provider authentication guide](provider-authentication.md#recorded-provider-profiles-0240) and [approval records](agent-instructions.md#approval-records). No adapter or model tool executed. |
| C14 (0.25.0) | Approval-gated model command tool | Done: see [model command tool](#model-command-tool) and ADR-0040. A CLI acceptance test qualifies one successful approved command on both hosts; confinement correctness remains the platform suites' responsibility. |

Later work needs its own decisions:

- registry credentials and development-server networking;
- model write, network, Git, and MCP tools;

Live qualification on each supported OS remains a separate gate.
