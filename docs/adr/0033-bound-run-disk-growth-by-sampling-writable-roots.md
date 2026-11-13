# 0033: Bound run disk growth by sampling writable roots

- Status: Accepted
- Date: 2026-11-13

## Context

Trusted runs have kernel-enforced memory and process limits, polled CPU-time
limits, and, since 0.18.0, optional outbound networking. They had no disk
limit: a confined tree could fill any volume its Landlock or AppContainer rules
let it write.

No portable per-run disk quota exists for an unprivileged runner.

- Windows Job Objects expose memory, process, and CPU accounting, but no disk
  quota. NTFS per-user quotas need administrative setup on each volume, and the
  run's writes are not attributed to a separate user.
- A cgroup v2 has no disk controller. `io.max` limits throughput, not total
  bytes.
- Filesystem project quotas need mount options and privileged setup. A tmpfs
  size limit needs a mount namespace and is not available to the confined tree
  without privileges.
- `RLIMIT_FSIZE` exists on Linux only, limits one file rather than the tree,
  and has no Windows equivalent.

The user selected a disk limit as the next execution increment. It must fail
closed, must not claim a quota the runner cannot enforce, and must keep the
existing rule that every trusted run has all limits.

## Decision

Every trusted run records and applies `TreeLimits::max_disk_bytes`. The CLI
flag is `--max-disk-mib` (default 8192, range 16 to 1048576). A trusted run
without a disk limit is refused before it is recorded.

The supervisor enforces the limit by sampling, not by a filesystem quota:

- Before the program starts, it measures the allocated bytes and the file
  lengths under the confinement's writable paths. Those totals are the
  baseline.
- A dedicated `omgcode-disk` thread repeats both measurements once a second
  while the run lives. Each total has its own budget of `baseline + limit`; if
  either passes, it reports the limit; the supervisor terminates the whole
  tree and records `limit_exceeded` with `disk`.
- The final measurement runs after the tree is observed empty, so a program
  that wrote past the limit and exited before the next sample still fails.
- The recorded `usage.peak_disk_bytes` is the larger of the two net growths,
  each floored at zero. Tracking lengths catches writes before the filesystem
  writes back delayed allocation; tracking allocation catches a pre-existing
  sparse file being filled in.

The measured roots are the confinement's `read_write` directories and regular
files, except character devices such as `/dev/null`. On Windows the
container's profile folder (`%LOCALAPPDATA%\Packages\<name>\AC`) is also
measured because the container writes there without a grant. Nested roots are
counted once.

Measurement is fail-closed. A baseline that cannot be measured records
`not_started`. While the run lives, a root that cannot be read, or more than
4,000,000 entries, ends the run as a supervision failure instead of claiming a
limit that was not measured. Symlinks and other reparse points are not
followed. A first error is reported once and stops the tree.

The limit is sampled and best-effort in these documented senses:

- A tree can pass the limit between samples. The overshoot is bounded by the
  write rate and the sample interval, not by the kernel.
- Growth is net. Bytes the tree deletes are subtracted, so a tree that deletes
  and rewrites the same bytes can churn more than the limit without adding
  total use.
- Two totals are measured: allocated bytes and file lengths. The limit applies
  to each separately. The measurement reads `st_blocks` on Linux and the
  compressed/allocated size on Windows, plus the file length. Directory
  entries, inode or MFT records, cluster slack, and filesystem metadata are
  not counted. A hard-linked file is counted once per name, so several names
  overcount it.
- Other writers to the same roots are counted as the run's growth. An operator
  who needs attribution to the run must not allow concurrent writers.
- The sample walks the roots on its own thread. A large workspace makes each
  sample slower and adds metadata I/O; it does not block deadline,
  cancellation, output, or cleanup checks.

## Alternatives considered

- **No disk limit.** Rejected: it leaves the only unstated resource bound in
  the execution contract and lets a build fill the host volume.
- **A Windows Job Object limit or cgroup disk controller.** Rejected: neither
  exists.
- **NTFS quotas or filesystem project quotas.** Rejected for the prototype:
  both need administrative setup, per-volume state, and a separate identity for
  the run, none of which the unprivileged runner has.
- **A tmpfs-backed scratch with a size limit.** Rejected: it needs mount
  namespace privileges, covers only scratch rather than the workspace, and
  would force ordinary workspace writes elsewhere.
- **`RLIMIT_FSIZE`.** Rejected: Linux only, per file rather than per tree, and
  it cannot bound many files.
- **Kernel I/O accounting or eBPF.** Rejected for now: privileged setup and a
  larger unsafe surface than the sampled supervisor.

## Consequences

- The remaining trusted-run resource limit is implemented, and the run journal
  records the limit and peak growth. A run journal from 0.19.0 carries
  `max_disk_bytes` and `peak_disk_bytes`; 0.18.0 cannot read it, as with
  earlier added fields.
- Every trusted run pays one baseline walk and one walk per second. The
  baseline walk delays the start; the periodic walk runs beside supervision.
- The limit prevents ordinary runaway build output and large-file copies. It
  does not prevent inode or directory-entry exhaustion, and it is not a quota.
  The owning guide must keep those limits visible and must not describe the
  result as kernel-enforced.
- A run that makes a writable directory unreadable to the supervisor fails as
  a supervision error. Landlock does not restrict `chmod`, so this is a
  deliberate fail-closed result rather than a bypass.
- Live qualification remains a separate gate. The tests run on Windows 11
  build 26300 and WSL 6.6; they do not qualify other hosts or filesystems.

## References

- [Command execution](../execution.md#tree-limits)
- [Development tools](../development-tools.md)
- [Security coding rules](../security-coding-rules.md#process-and-sandbox-boundary)
- [Testing standards](../testing-standards.md)
- [ADR-0031](0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md)
- [ADR-0032](0032-allow-outbound-network-for-trusted-runs-through-task-network.md)
