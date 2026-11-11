# 0028: Confine Linux runs with Landlock and a socket filter

- Status: Accepted
- Date: 2026-11-11

## Context

[ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
selected Landlock and seccomp for Linux confinement. C1 added an unconfined
process-tree core. C2 must add read-only toolchain roots, writable
workspace and scratch paths, denied host data, denied IP sockets, a decision on
Unix-domain sockets to host services, and Landlock ABI detection that fails
closed.

Established facts that shaped the decision:

- Landlock ABI 1 and 2 do not handle `LANDLOCK_ACCESS_FS_TRUNCATE`, so a
  read-only file could still be truncated by path. ABI 3 handles it. The test
  kernel (WSL 6.6) reports ABI 3; Ubuntu 24.04's 6.8 kernel is documented as
  ABI 4.
- Landlock ABI 4 adds TCP bind/connect rules only. It does not cover UDP, raw,
  netlink, or Unix-domain sockets. ABI 6 scopes abstract Unix sockets and
  signals. No ABI up to 6 controls `connect` to a pathname Unix socket, so
  Landlock alone cannot keep a program away from the Docker daemon, the D-Bus
  session bus, or an SSH agent.
- `io_uring` can create sockets without the `socket` system call.
- seccomp can inspect register arguments, such as the `socketpair` domain and
  type, but not a destination address passed through `sendmsg`.

## Decision

`omgcode_platform::process::run_confined` applies a caller-supplied
`Confinement` on Linux. The [execution guide](../execution.md#linux-confinement)
owns the details.

- **Filesystem:** a Landlock ruleset with read-only paths (execute, read, list)
  and read-write paths (every handled right except creating device nodes).
  Everything else is denied. The parent opens each path with `openat2` and
  `RESOLVE_NO_SYMLINKS`, accepts only directories, regular files, and character
  devices, and builds the ruleset before `fork`. The child only calls
  `landlock_restrict_self` after `no_new_privs`.
- **ABI:** ABI 3 is the minimum. Below it, or without Landlock, nothing starts.
  ABI 4 and later also deny TCP through Landlock; ABI 5 and later handle device
  ioctls; ABI 6 and later scope abstract Unix sockets and signals. The outcome
  records the ABI used.
- **Network:** the seccomp filter refuses `socket` and `socketpair` for every
  address family other than `AF_UNIX`, and refuses `io_uring_setup`,
  `io_uring_enter`, and `io_uring_register`, all with `EPERM`. The filter is
  inherited and cannot be removed.
- **Unix-domain sockets:** the user chose a per-run `UnixSocketAccess` mode with
  the stricter mode as the default:
  - `PairsOnly` (default): `socket(AF_UNIX)` is refused. Only `socketpair` of
    stream or sequenced-packet type is allowed, for IPC inside the tree.
    Datagram pairs are refused because `sendto` on one can reach a host
    socket path such as `/dev/log`.
  - `HostServices`: `socket(AF_UNIX)` and every Unix `socketpair` are allowed,
    and read-write paths may hold socket files. The tree can reach every
    pathname socket the user can reach. This exists for developers who need
    Docker or similar services, and must be an explicit operator choice.
- The outcome reports `AppliedConfinement::Landlock { abi, unix_sockets }`.
  Windows refuses `run_confined` with `Unsupported` until C3.

## Alternatives considered

- **Pairs only, with no relaxed mode:** strongest default, but leaves no
  supported path for developers whose builds need the Docker daemon. The user
  asked for both modes with the strict one as the default.
- **Allowing `socket(AF_UNIX)` by default:** better compatibility, but every
  host service the user can reach becomes reachable. Rejected as the default.
- **Denying all Unix sockets, including pairs:** breaks common runtimes, such as
  Python `asyncio` and `multiprocessing`, that use `socketpair` internally.
  Rejected.
- **Accepting Landlock ABI 1 or 2:** works on older kernels, but a read-only file
  could be truncated. Rejected; nothing starts instead.
- **Landlock TCP rules instead of seccomp:** cover only TCP and need ABI 4.
  Kept as defence in depth where available, not as the network control.
- **The `landlock` crate:** maintained and convenient, but its best-effort
  defaults can silently weaken a ruleset, and its error paths allocate, which is
  unsafe between `fork` and `exec` in a multithreaded parent. The raw ABI is
  three system calls and two structures, so the platform crate defines them
  directly.
- **Following symbolic links in confinement paths:** convenient for merged-`/usr`
  aliases such as `/lib`, but a link could redirect a rule to another tree.
  Rejected; callers pass the real path.

## Consequences

- Linux confined execution needs kernel Landlock ABI 3 (Linux 6.2 or later)
  with Landlock enabled, in addition to C1's requirements.
- A confined program can use no IP networking at all, including loopback. A
  package-network profile needs a later, separate decision.
- In the default mode, programs that create named or abstract Unix sockets
  fail with `EPERM`. `HostServices` relaxes that but exposes every reachable
  pathname socket; connecting to the Docker daemon is usually equivalent to
  root on the host.
- Before ABI 6, a confined program can still signal other processes of the same
  user and connect to abstract Unix sockets in `HostServices` mode.
- Landlock does not restrict reading metadata through `stat`, following
  `readlink`, or changing ownership and permissions of reachable files.
- Tests ran on WSL 6.6 (ABI 3). Each of eight temporary mutations, such as
  removing `landlock_restrict_self`, allowing `socket`, allowing `io_uring`, not
  handling truncation, or following symbolic links, was caught. Nothing calls
  `run_confined` yet; C4 adds the CLI caller. This is not a qualification on
  other kernels or distributions.

## References

- [Execution guide](../execution.md)
- [ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
- [Landlock userspace API](https://docs.kernel.org/userspace-api/landlock.html)
- [seccomp filter](https://docs.kernel.org/userspace-api/seccomp_filter.html)
