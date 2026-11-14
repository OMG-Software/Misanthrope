# 0030: Run trusted commands through recorded toolchain profiles

- Status: Accepted
- Date: 2026-11-11

## Context

[ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md) and
[ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
added `run_confined` on Linux and Windows, with no caller. Increment C4 must
add a trusted operator `run` behind the workspace `execute` preference. It
needs:

- toolchain profiles and a curated environment;
- the profile recorded with each result;
- journal records;
- an explicit recorded choice before `HostServices`;
- Windows confinement built from the recorded grants;
- the development-tool acceptance cases.

The user chose these options for C4:

- **Profiles:** operator-declared. Trusted CLI add, show, and remove commands
  declare explicit read-only roots, `PATH` entries, and environment values.
  They are recorded in the control journal with a revision. There is no
  automatic discovery.
- **Run records:** a separate bounded run journal beside the control journal.
  It records the intent before the start and the outcome afterwards: the
  profile, the program, an argument digest, and the result. Output goes back to
  the CLI and is not stored. An explicit command archives the journal.
- **Host services:** a new `host_services` workspace preference, off by
  default and changed with a revision, together with a per-run
  `--host-services` flag. Both are required. The run record notes the mode.
  Windows still refuses it.
- **Acceptance:** real Rust, Python, Node, C/C++, Go, and Java workflows on
  Windows and Linux.

Facts observed during the work, on Windows 11 build 26300 and WSL 6.6:

- Node's CommonJS loader reads the attributes of every ancestor directory,
  starting at the drive root. An AppContainer cannot read the drive root or
  the user profile, so `node script.js` fails with `EPERM` before user code
  runs. `--preserve-symlinks --preserve-symlinks-main` skips that walk.
- Inside an AppContainer, Node child processes with piped standard streams
  never return, even `spawnSync` with a timeout. With inherited streams they
  work. The pipe names libuv chooses are evidently outside the container's
  namespace. This cause was inferred and not traced.
- Ubuntu's Node reads `/usr/lib/ssl/openssl.cnf`, which links into
  `/etc/ssl`. Without that directory as a root, Node exits at start.
- Python, Go, Rust, MSVC `cl` and `link`, and GCC worked under both
  confinements with only their installation roots, plus the MSVC and SDK
  directories on Windows.

## Decision

`omgcode run` and `omgcode runs` are trusted CLI commands. The core function
`omgcode_core::run::run` checks the live settings, records the intent, calls
`run_confined`, and records the outcome. The
[execution guide](../execution.md#trusted-runs) owns the details.

- **Profiles:** each profile has a name, the platform it was declared on, one
  or more read-only roots, `PATH` entries inside those roots, and extra
  variables. Each directory is recorded by its identity.
  - **Refused:** roots that overlap the development root in either direction
    or overlap application storage; reserved variable names such as `PATH`,
    `HOME`, `TEMP`, and `LOCALAPPDATA`; secrets the profile would need.
  - **Events:** `profile_added` and `profile_removed`. Both need the expected
    revision and advance it.
  - **Changing a profile:** remove it and add it again; there is no edit.
  - **Stale directories:** a run refuses a profile whose directories were
    moved or replaced.
- **Program resolution:**
  - A bare name is searched in the profile's `PATH` entries, never the
    caller's.
  - A relative path with a separator names a file in the workspace.
  - An absolute path must lie inside a profile root or the workspace.
  - The resolved file's directory must be inside a root or the workspace.
- **Environment:** the run receives only:
  - `PATH` built from the profile;
  - runner-owned variables, which are on Linux `HOME`, `TMPDIR`, and
    `XDG_CACHE_HOME` pointing to a private per-run scratch directory, and on
    Windows `LOCALAPPDATA`, `SystemRoot`, `windir`, and `HOME` and
    `USERPROFILE` pointing to the container's folder;
  - the profile's variables.

  Nothing else is inherited.
- **Confinement:**
  - On Linux the read-only list is the profile roots plus three random and
    zero devices. The read-write list is the workspace, the scratch
    directory, and `/dev/null`.
  - On Windows the workspace must hold an active read-write grant. No grant of
    the workspace may be pending or being revoked. The roots are checked
    against their live ACLs by `run_confined`.
- **Run journal:** `<data-dir>/runs-v1.jsonl` has schema version 1, a store
  identity matching the control journal, and contiguous sequences. Its events
  are `created`, `run_started`, `run_finished`, and `run_acknowledged`.
  - **`run_started`:** synced before the process starts. It holds the
    workspace, settings revision, profile identity, name and revision, the
    program path, the argument count, a SHA-256 digest of the arguments, the
    working directory, and the socket mode. Argument text, environment values,
    and output are never written.
  - **`run_finished`:** records the result, the applied confinement, the
    elapsed time, and the byte counts.
  - **Pending runs:** a run with no outcome, or with unconfirmed cleanup,
    stays pending. `runs acknowledge` clears it. That claims no outcome and
    retries nothing. A run whose supervising process still holds its owner
    lock, `run-owners/<run id>.lock`, cannot be acknowledged.
  - **Limits:** 16 pending runs, 8 MiB, 4,096 events, and 64 KiB per event.
    Each append that adds work must leave room for every pending run's
    remaining events.
  - **Archiving:** `runs archive` moves a journal with no pending runs into an
    existing directory.
- **Host services:** the request flag, the workspace preference, and a Linux
  host are all required. The mode is recorded in `run_started`.
- **Locks:** the control and run journal locks are held only while checking
  and appending, never while the program runs. Concurrent runs and settings
  commands can proceed.
- **Preferences:** a run needs `execute`, `read`, and `write`, because it
  writes inside the workspace.
- **Revocation:** a watcher in the running process rereads the settings after
  each control journal change. When they no longer allow the run, it stops the
  tree and records `revoked`. Settings commands that can withdraw access wait
  for the affected runs to stop and record their outcome, and fail if one does
  not within 60 seconds. This follows the rule that narrowing permissions
  stops tasks that keep revoked access, and that revocation is reported only
  after cleanup is observed.
- **Interrupts:** the CLI turns `SIGINT` and `SIGTERM` on Linux, and Ctrl+C
  and Ctrl+Break on Windows, into cancellation. The tree is terminated and the
  outcome is recorded as `cancelled`.
- **Capabilities:** `capabilities` reports `process_execution:
  "trusted_cli_only"` and `toolchain_profiles: true` on Linux and Windows.
  Settings diagnostics keep `execution_allowed: false`, because they do not
  authorize a run. At that time, no model tool could start a process; ADR-0040
  later added an approval-gated command tool that calls the same runner.

## Alternatives considered

- **Automatic toolchain discovery:** convenient, but it would run probes and
  widen access from host state the operator did not review. The user chose
  operator-declared profiles.
- **Applying settings changes only to later runs:** simpler, but a running
  tree would keep access the operator had withdrawn. Rejected in review.
- **Signalling runs directly from the settings command:** needs a channel
  between processes and a list of live runs that can go stale. The watcher
  reads the same journal that records the change, and the owner lock shows
  which runs are live.
- **Recording runs in the control journal:** one log, but run traffic would
  use settings capacity and its lock. The user chose a separate journal, as the
  file operation journal already does.
- **Storing output:** useful for later review, but output can contain secrets
  and is unbounded in practice. The user chose to return it only.
- **A per-run flag alone for host services:** a typing slip could expose the
  Docker daemon. The user chose to also require a revisioned preference.
- **Inheriting the caller's environment, or a filtered copy:** that would
  carry credentials and host configuration into the run. Rejected. Only
  declared values pass.
- **Built-in workarounds for Node on Windows:** the runner could add Node
  options or patch stdio. Rejected: profiles stay declarative, and the
  operator sets `NODE_OPTIONS` in the profile. The limits are documented.

## Consequences

- An operator can run real builds, tests, formatters, and project programs in
  a confined workspace on Linux and Windows. Each run has a recorded intent
  and outcome.
- Profiles hold exactly what the operator declared. Tools that need a
  configuration directory outside their installation, such as `/etc/ssl` for
  a distribution Node, need that directory declared as a root.
- Profile variables sit in the private settings journal and `workspace status`
  shows them. They must not hold secrets.
- On Windows, Node needs `NODE_OPTIONS=--preserve-symlinks
  --preserve-symlinks-main` in its profile. It cannot use piped child
  processes, which includes the default isolated `node --test`. Use
  `--test-isolation=none` (Node 22.8 or later).
- There is still no IP networking, so package downloads fail unless
  dependencies are already present. There are no tree-wide memory,
  process-count, or disk limits.
- The Java acceptance case is written but has not run, because no JDK was
  installed on either test host.
- Tests ran on Windows 11 build 26300 and WSL 6.6 (Landlock ABI 3) only.

## References

- [Execution guide](../execution.md)
- [Development tools](../development-tools.md)
- [Workspace settings](../workspace-permissions.md)
- [ADR-0003](0003-use-scoped-development-toolchain-profiles.md)
- [ADR-0012](0012-separate-workspace-settings-from-execution-authority.md)
- [ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md)
- [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
