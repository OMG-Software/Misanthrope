# Changelog

Changes are recorded here before a release version is assigned.

## Unreleased

### Added

- Version 0.25.0 adds an approval-gated model command tool. See ADR-0040.
  - **Offering:** `workspace_run_command` is advertised only when the
    workspace's live `model_tools`, `execute`, `read`, and `write`
    preferences are enabled. Every call rechecks them; the trusted run checks
    them again.
  - **Arguments:** a recorded toolchain profile, program, at most 64
    arguments, and an optional workspace-relative `cwd`. There is no shell.
  - **Approval:** the first call records a pending approval and returns
    `approval_required`; it starts nothing. After `approvals approve <id>`, a
    second identical call consumes the approval and calls the trusted runner.
    The model cannot approve, select limits, or select network/host services.
  - **Limits:** 20-second deadline, 4 KiB per output stream, 1 GiB memory,
    64 processes, 20 seconds of CPU, and 256 MiB of disk growth.
  - **Records:** the session journal records the tool intent/result; the run
    journal records the run. A failed run consumes the approval; no replay
    executes a recorded call.
  - **Qualified:** a successful approved command through the real chat CLI on
    both hosts. A mock provider requests the platform shell's `echo`; the first
    turn records `approval_required`, the operator approves, and the second
    identical turn consumes the record and runs the command through the trusted
    runner. The test asserts an `exited` result with code 0, the bounded output,
    the session tool result, and the consumed approval. On Linux the CLI starts
    in a delegated cgroup; on Windows the scene grants the workspace.
- Version 0.24.0 records provider profiles and trusted per-call approvals.
  See ADR-0038 and ADR-0039.
  - **Provider profiles:** `workspace provider-profile-add/show/remove` record
    a name, provider kind, and authentication mode (`subscription`,
    `api_key`, or `none`). Subscription is limited to OpenAI and Anthropic;
    local providers use `none`. No endpoint, account, token, or credential is
    stored, and no adapter or login uses a profile. `capabilities` reports
    `provider_profiles: true` and `provider_adapters: false`.
  - **Approvals:** `approvals request/list/show/approve/deny/consume` manage
    trusted, single-use records bound to a session, workspace, operation,
    exact request digest, and settings revision. Records expire in 60 to 3600
    seconds; a settings change invalidates them; no argument text is stored.
    The CLI consume command executes nothing, and no model tool consumes an
    approval yet.
  - **Records:** `provider_profile_added`, `provider_profile_removed`,
    `approval_requested`, `approval_decided`, and `approval_consumed` in the
    control journal. A 0.24.0 journal cannot be read by 0.23.0.
- Version 0.23.0 adds a per-workspace package cache root and scoped Windows
  access. See ADR-0037.
  - **Commands:** `workspace set-package-cache <workspace-id> <dir>
    --revision <n>` and `workspace clear-package-cache <workspace-id>
    --revision <n>`. `status` lists roots under `package_cache_roots`.
  - **Rules:** the root must exist, must not overlap application storage or
    the development root, and must not overlap another workspace's root.
  - **Fetch:** `packages fetch` writes into the selected root. Without one,
    Linux keeps the application-storage fallback and Windows has none.
  - **Runs:** a selected root is exposed read-only and named by
    `OMGCODE_PACKAGE_CACHE` on both platforms. Windows requires a read-execute
    container grant on that root and traversable ancestors; without the grant
    the run fails before the program starts. `run_started` records
    `package_cache` when true.
  - **Records:** `package_cache_root_set` and `package_cache_root_cleared` in
    the control journal. A 0.23.0 control journal cannot be read by 0.22.0.
  - **Limits:** no invalidation, eviction, or freshness check; changing the
    root leaves old files and old grants for the operator to manage.
- Version 0.22.0 exposes the fetched-package cache to Linux runs read-only.
  See ADR-0036.
  - **Linux:** when `<data-dir>/package-cache/<workspace-id>` exists as a
    plain directory, the run gets it read-only and `OMGCODE_PACKAGE_CACHE`.
    The parent, journals, settings, and other workspaces stay denied; the run
    cannot write the cache.
  - **Windows:** the cache is not exposed. The AppContainer cannot read
    application storage without an ACL grant, and container grants refuse
    that storage.
  - **Records:** `run_started` records `package_cache` only when true. A 0.22
    journal that records it cannot be read by 0.21.0.
  - **Reserved:** `OMGCODE_PACKAGE_CACHE` cannot be declared in a toolchain
    profile.
  - **Limits:** no invalidation, eviction, or freshness check; configuring a
    toolchain to use the directory is operator work.
- Version 0.21.0 adds the trusted operator command `omgcode packages fetch`
  and a private allowlisted package cache. See ADR-0035.
  - **Command:** `packages fetch <workspace-id> --profile <name> --url <url>
    --name <cache-file> [--max-bytes <n>] [--timeout-secs <n>]`.
  - **Gating:** trusted CLI only; never a model tool. The workspace must be
    registered and enable `task_network`.
  - **Destination:** the URL must match a recorded origin's scheme, host,
    port, and path prefix. Normal use is `https`; `http` is accepted only for
    loopback tests. Credentials, queries, fragments, and redirects are
    refused. Proxy environment variables are ignored.
  - **Transport:** system TLS trust through `reqwest`'s `native-tls` feature;
    certificate validation is never disabled.
  - **Bounds:** one GET, 1 byte to 1 GiB, 1 to 600 seconds. The body is
    streamed, hashed with SHA-256, and stopped at the limit.
  - **Cache:** `<data-dir>/package-cache/<workspace-id>/<cache-file>`, private
    directories and files, exclusive name, temporary file then rename. No
    power-loss claim. The run does not yet read the cache.
  - **Audit:** `package_fetch_started` and `package_fetch_finished` in the
    control journal preserve the settings revision and omit the URL text. A
    control journal written by 0.21.0 cannot be read by 0.20.0.
- Version 0.20.0 records operator-declared package-network profiles as
  configuration only. See ADR-0034.
  - **Commands:** `workspace package-profile-add`, `package-profile-show`, and
    `package-profile-remove` use the expected settings revision. `status`
    lists recorded profiles.
  - **Contents:** a name and 1 to 8 registry origins. An origin is `https`
    with a host, optional port, and optional path prefix; `http` is accepted
    only for `localhost`, `127.0.0.1`, or `::1`. Credentials, queries,
    fragments, wildcards, and path traversal are refused.
  - **Not enforcement:** no run accepts a profile, `--network` remains
    all-or-nothing, and no destination filter exists. A profile never
    authorizes network access or a run.
  - **Records:** `package_profile_added` and `package_profile_removed` events
    in the control journal. A control journal written by 0.20.0 cannot be read
    by 0.19.0; 0.20.0 reads earlier journals.
- Version 0.19.0 bounds each trusted run's net disk growth. See ADR-0033.
  - **Flag:** `--max-disk-mib` (16 to 1048576, default 8192) is required for
    every trusted run. A run without a disk limit is refused before anything
    is recorded.
  - **Enforcement:** the supervisor measures allocated bytes and file lengths
    under the confinement's writable roots before the program starts and once
    a second on a separate thread. Each total has its own budget; passing
    either ends the whole tree and records `limit_exceeded` with `disk`. A
    final measurement runs after the tree is empty. The limit is sampled; it
    is not a filesystem quota.
  - **Fail closed:** a baseline that cannot be measured records `not_started`.
    An unreadable root, or more than 4,000,000 entries, while the run lives
    ends it as `supervision_failed`. Links and reparse points are not
    followed. Other writers to the roots count as the run's growth.
  - **Records:** `run_started` records `max_disk_bytes`, and the `usage` field
    of `run_finished` records `peak_disk_bytes`. A run journal written by
    0.19.0 cannot be read by 0.18.0; 0.19.0 reads 0.18.0 journals.
  - **Not counted:** directory entries, inode or MFT use, and filesystem
    metadata. A hard-linked file is counted once per name. Tested on Windows
    11 build 26300 and WSL 6.6 only.
- Version 0.18.0 adds outbound network access for trusted runs. See ADR-0032.
  - **Gating:** `omgcode run ... --network` needs the workspace
    `task_network` preference. Without both, a run has no IP networking, as
    before. Turning the preference off stops networked runs, recorded as
    `revoked`.
  - **Linux:** the socket filter allows `AF_INET` and `AF_INET6` sockets.
    Every destination is reachable, including loopback and the local
    network. On Landlock ABI 4 and later, an explicit TCP bind stays denied,
    but `listen` on an unbound socket can still accept connections. The run
    can read a fixed list of resolver files and public certificate stores,
    never the private-key directories under `/etc/ssl` and `/etc/pki`.
  - **Windows:** the AppContainer gets the `internetClient` capability only.
    Loopback and private networks stay blocked.
  - **Records:** `run_started` records `network` with the support paths.
    Records written by 0.17.0 still replay.
- Version 0.17.0 limits every trusted run as a whole tree. See ADR-0031.
  - **Limits:** `--max-memory-mib` (default 8192), `--max-processes`
    (default 1024), and `--max-cpu-secs` (default 7200). Values out of range
    are refused before anything is recorded.
  - **Windows:** the run's job object gets a job memory limit and an active
    process limit; CPU time comes from the job's accounting.
  - **Linux:** OMGCode must run in a delegated cgroup v2, for example under
    `systemd-run --user --scope -p Delegate=yes`. Each run gets its own child
    cgroup with `memory.max`, no swap, group OOM kill, and `pids.max`. A run
    outside a delegated cgroup is refused before it is recorded.
  - **Result:** reaching a limit ends the whole tree and records
    `limit_exceeded` with the limit. `run_started` records the limits;
    `run_finished` records CPU time and, where available, peak memory.
    Records written by 0.16.0 still replay.
- Version 0.16.0 adds the trusted CLI command `omgcode run <workspace-id>
  --profile <name> -- <program> [arguments]`. It runs one program in a
  workspace whose `execute` preference is enabled.
  - **Confinement:** always through `run_confined`. Linux uses Landlock and the
    socket filter, with a private per-run scratch directory as `HOME` and
    `TMPDIR`. Windows uses the workspace AppContainer and needs an active
    read-write grant on the workspace.
  - **Program and environment:** the program is found through the profile's
    `PATH` entries, in the workspace, or inside a profile root. The run
    receives only the profile's `PATH`, runner-owned home and temporary
    variables, and the profile's variables.
  - **Options:** `--cwd`, `--timeout-secs` (default 600), and
    `--max-output-bytes` (default 1 MiB). Ctrl+C or `SIGTERM` cancels the
    whole tree.
  - **Preferences:** a run needs `execute`, `read`, and `write`.
  - **Revocation:** while a program runs, a watcher rereads the settings after
    each change. When they no longer allow the run, it stops the whole tree
    and records `revoked`. `workspace permissions`, `profile-remove`,
    `container-revoke`, and `set-root` wait up to 60 seconds for the runs they
    affect to stop, and fail if one does not.
  - **Output:** printed, never stored.
- New trusted commands `workspace profile-add`, `profile-show`, and
  `profile-remove` manage operator-declared toolchain profiles.
  - **Contents:** a name, read-only roots, `PATH` entries inside them, and
    extra variables.
  - **Recording:** each change needs the expected revision and is recorded in
    the control journal by directory identity.
  - **Refused:** roots that overlap the development root or application
    storage; reserved variable names.
  - **Stale profiles:** a moved or replaced directory makes the profile stale,
    and runs refuse it. Remove it and add it again.
- A separate run journal, `runs-v1.jsonl`, records each run's intent before it
  starts and its outcome afterwards.
  - **Recorded:** the profile and its revision, the program, an argument count
    and SHA-256 digest, the working directory, the socket mode, the result,
    the confinement, and byte counts. Argument text and output are not
    recorded.
  - **Pending runs:** a run with a missing outcome or unconfirmed cleanup
    stays pending until `runs acknowledge`. A run whose supervising process is
    still live cannot be acknowledged. `runs audit` lists records, and
    `runs archive` moves a journal with no pending runs.
- A `host_services` workspace preference, off by default, lets a Linux run
  select host Unix-socket access when the run also passes `--host-services`.
  Windows refuses it. Settings logs that record a profile or enable
  `host_services` cannot be read by 0.15.0.
- `capabilities` reports `process_execution: "trusted_cli_only"`,
  `toolchain_profiles`, and the sandbox backend. Settings diagnostics still
  report `execution_allowed: false`, and no model tool starts a process. See
  ADR-0030 and `docs/execution.md`.
- Toolchain acceptance tests, ignored by default, run real Rust, Python, Node,
  C/C++, Go, and Java workflows through `omgcode run`. Each covers builds,
  tests, formatting where available, subprocesses, denied host data, writes
  outside the workspace, loopback connections, and missing dependencies. All
  but Java passed on Windows 11 build 26300 and WSL 6.6; Java has not run
  because no JDK was installed.
  - **Node on Windows** needs `NODE_OPTIONS=--preserve-symlinks
    --preserve-symlinks-main` in its profile. Child processes with piped stdio
    hang in the container, so `node --test` needs `--test-isolation=none`.
  - **Ubuntu's Node** needs `/etc/ssl` as a profile root.
- Version 0.15.0 adds Windows AppContainer confinement to
  `process::run_confined`, for tests only. Each workspace has one container,
  `OMGCode.Workspace.<id>`, with no capabilities, so it has no network access,
  including loopback. It reaches only directories granted to it and paths that
  every application package can read, such as the Windows directory. Before
  anything starts, Windows checks that the container profile exists, that
  `LOCALAPPDATA` is in the environment, that each listed path and the program
  are granted at their level, and refuses `HostServices` with `Unsupported`.
  New trusted CLI commands, `workspace container-grant` and
  `workspace container-revoke`, give and remove persistent ACL grants:
  read-write only on the workspace directory, read-execute on the workspace,
  inside it, or outside the development root. The control journal records each
  intent before the ACL change and each outcome after it, keeps unconfirmed
  grants visible and revocable, and keeps room to revoke every outstanding
  grant when it is otherwise full. The first grant creates the container
  profile and revoking the last one deletes it. `workspace status` lists
  outstanding grants and `capabilities` reports `workspace_container_grants`.
  Settings logs that record a grant cannot be read by 0.14.0. A grant also
  reaches hard-linked files under their other names. Nothing runs in the
  container from the CLI yet. See ADR-0029 and `docs/execution.md`.
- Version 0.14.0 adds `process::run_confined` to the platform crate, for tests
  only. On Linux it applies a Landlock filesystem allowlist of read-only and
  read-write paths and a seccomp filter that refuses IP and other non-Unix
  sockets and `io_uring`. Landlock-handled filesystem operations are denied
  outside the listed paths; metadata changes such as `chmod` and `chown`, and
  device `ioctl` before ABI 5, are not restricted. Descendants inherit the
  rules. It needs Landlock ABI 3 or later and starts nothing
  otherwise, or when a path is missing, is a symbolic link, or has an
  unsupported type. A per-run `UnixSocketAccess` mode defaults to `PairsOnly`,
  which allows only stream and sequenced-packet `socketpair`; `HostServices`
  also allows connecting to host Unix sockets such as the Docker daemon and is
  for explicit operator use. The outcome records the Landlock ABI and the mode.
  Windows returns `Unsupported` until AppContainer confinement exists. No CLI
  command or model tool calls it. See ADR-0028 and `docs/execution.md`.
- Version 0.13.0 adds a process-tree runner core to the platform crate, for
  tests only. It starts one absolute-path program with an exact environment,
  null input, and bounded output. It ends the whole tree on exit, deadline, or
  cancellation and reports success only after observing the tree empty.
  Windows assigns a Job Object at creation and inherits only the standard
  handles. Linux uses a process group, a seccomp filter that denies `setsid` and
  `setpgid`, a parent-death signal, and close-on-exec descriptors. It applies no
  filesystem or network confinement, and no CLI command or model tool can call
  it. The accepted confinement is Landlock and seccomp on Linux and
  AppContainer on Windows; see ADR-0027 and `docs/execution.md`.
- Version 0.12.0 lets chat offer the local LM Studio model two tools,
  `workspace_read_file` and `workspace_list_directory`, for a workspace whose
  new `model_tools` preference is enabled. The preference is off by default and
  is set with `workspace permissions --model-tools true`. Settings logs that
  never enable it stay readable by 0.11.0. Each call rechecks the live
  `model_tools` and read preferences and stays inside the registered workspace;
  parent, absolute, rooted, and link paths are refused. There is no per-call
  approval prompt. A turn makes at most four model requests within 120 seconds.
  Tool results are source-labelled user messages, and the configured server
  token is removed from them before they are recorded or sent. `workspace
  status` reports `model_tools`, and `capabilities` reports
  `model_read_list_tools` and a new `tool_policy` value. Live LM Studio
  tool-call compatibility is not qualified. One opt-in Windows spot-check with
  LM Studio 0.4.28 and `google/gemma-4-e4b` passed; see ADR-0026. Saved tool
  results are sent again only while the live `model_tools` and `read`
  preferences allow it; otherwise, and in every text-only turn, each is replaced
  by a withheld notice and counted in `withheld_tool_results`. A tool call that
  contains the server token is refused before it is recorded. Tool-enabled
  output reports the final `finish_reason`, and platforms other than Linux and
  Windows stay text only with `platform_unsupported`.
- Version 0.11.0 adds trusted `file restore` and
  `file delete-checkpoint --permanent` on Linux ext4 and Windows NTFS. Restore
  writes a retained checkpoint's original bytes back through the normal apply
  path. It requires the current file's digest and size and a stopped-writers
  acknowledgement, and it records the replaced bytes in a new checkpoint.
  Deletion removes one resolved checkpoint's fixed files without recursion. It
  keeps the operation's history and frees its retained slot. An interrupted
  deletion blocks other file work until the same request finishes it. Version
  0.10.0 refuses a ledger that contains deletion records. `capabilities` reports
  `checkpoint_restore` and `checkpoint_deletion`. Neither command is power-loss
  qualified; see ADR-0025.
- Version 0.10.0 enables checkpoint storage and trusted `file apply` on Windows
  NTFS. Apply preserves the owner, group, DACL, and content-indexing opt-out, and
  refuses metadata it cannot reproduce. The stage stays owner-only until the
  commit attempt, and a failure before the rename tries to restore that DACL.
  The opt-out changes only while the stage is private, so a crash between the
  two metadata writes leaves a stage that recovery accepts.
  A crash or failed restore in that window can leave a retained stage with the
  source DACL, which may expose unpublished bytes to principals it admits.
  Windows makes no power-loss claim and is not qualified for release; see
  ADR-0024.
- Add a Windows NTFS private-store backend with exact protected per-user DACLs,
  handle-relative access, and a failing directory flush. Microsoft documents no
  durability contract for the flush; see ADR-0023.

### Changed

- Package-network destination enforcement is defined as a contract only in
  ADR-0034. No run accepts a package-network profile, and `--network` remains
  all-or-nothing. An advisory proxy or proxy environment variables are
  explicitly rejected because a confined tree can bypass them. This is a
  documentation change; no application version is bumped.
- The SonarQube Cloud project key is now `OMG-Software_OMGCode`, matching the
  key a Sonar administrator set on the server. `sonar-project.properties` and
  the repository-checks guide use the new key.
- `workspace` settings commands wait up to 10 seconds for another command's
  control lock instead of failing at once. Run watchers hold that lock briefly
  while they reread the settings.
- `cancel_on_interrupt` returns an `InterruptWatch` that stops and joins its
  watcher thread, instead of leaving a detached thread for the life of the
  process.
- Rename the project and GitHub repository to OMGCode. Version 0.7.0 uses
  `omgcode-*` Rust crates, the `omgcode` command, `OMGCODE_LM_TOKEN`, the OMGCode
  application-data identity, and `omgcode-` session filenames. Prompt version
  0.1.4 carries the new identity. Old names have no runtime aliases. Existing
  private data stays in place; explicit transition instructions are in the
  development guide. Keep the existing Sonar project key to retain its history.

### Fixed

- Tree limits, after the PR #27 review:
  - **Range checks before recording:** the core checks every run limit before
    it records a run, so a direct caller with an out-of-range tree limit gets
    `InvalidLimits` and nothing is recorded.
  - **Nested Windows jobs:** a job nested in the run's job, such as a tool's
    own stricter job, can send its limit messages to the run's port. The
    runner now counts a message only when the run's own job confirms it. Each
    poll takes at most 256 messages. Once the program has exited, the runner
    takes up to 64 batches before it stops the tree, so a limit queued behind
    other messages still decides the result. If messages remain after that,
    the run is recorded as `supervision_failed`, not as a normal exit. A
    nested job's memory message
    can still end a run already within a sixteenth of its own memory limit,
    because Windows does not identify the sending job.
  - **Linux termination:** a failed `cgroup.kill` no longer skips the process
    group kill; both are tried and the first failure is reported.
  - **Cleanup:** the run's cgroup or job limit state is released even when its
    usage cannot be read.
  - **Grant revocation:** an unreadable run journal no longer blocks
    `container-revoke`. The grant is revoked, then the command fails and says
    so.
- Settings changes now confirm that withdrawn runs stopped, as the PR #26
  review asked:
  - **Unconfirmed cleanup:** a pending run whose cleanup was unconfirmed may
    still have processes. A settings change that withdraws it now fails and
    lists it, instead of reporting success.
  - **Acknowledged runs:** a run counts as stopped only when an outcome other
    than `cleanup_unconfirmed` is recorded. A run acknowledged without an
    outcome while the command waited no longer counts.
  - **Windows grants:** revoking any container grant of a workspace now stops
    that workspace's runs, not only revoking its read-write grant. The run's
    watcher compares a per-workspace revocation count, so it cannot miss a
    revocation between two polls. `container-revoke` waits for each run of the
    workspace that started before the revocation.
- Run workflow tests now allow settings commands their full 60-second wait,
  keep grants whose revocation failed so the fallback can retry them, and on
  Linux end the process group of the abandoned run before the test finishes.
- The Windows runner test for escaping descendants now allows up to five
  seconds for a killed grandchild's process object to become signaled. Windows
  can report the job empty before that object is signaled; the instant check
  failed intermittently on loaded CI runners, including `main` at `6b1ebe4`. A
  grandchild that survives still fails the test.
- Stop parallel CLI integration tests from failing with `Busy` on Linux. A child
  spawned while a test held a store open could inherit that store's `flock` until
  `exec`, so the lock outlived the test's close. A test-only gate now keeps child
  spawns and in-process store sections apart. Production locking is unchanged.

- Use a typed checkpoint inspection state while preserving its JSON values.
  Clarify that checkpoint admission cannot detect complete erasure of both
  checkpoint paths. Document persistence helper contracts and the acceptance
  scenarios for checkpoint storage and recovery.

- Harden the core turn/tool runtime: reserve assembled-prompt capacity and
  enforce the selected tool-message limit before recording results, retain
  interim assistant text in follow-up history, preserve original policy
  binding errors, and cover failed-turn call-identifier reuse in replay.

- Keep operational file checks out of the settings journal so file traffic and
  audit-cap refusals cannot exhaust capacity needed for permission changes.
- Add explicit, no-replace operation-journal exports and read-only archive
  inspection; preserve raw bytes and refuse unresolved logs.
- Reject unusable workspace scopes during registration/binding, classify invalid
  target paths as request errors, and report unchecked broker availability honestly.
- Continue Linux listings when entries disappear and distinguish definite mkdir
  permission refusals from uncertain mutations. Add synchronized pinned-read,
  bounded-consumption, archive-fault, CLI recovery, and capacity regressions.
- Clarify recovery prerequisites, export/privacy limits, and credentials in names.

- Reject stale saved workspace paths before reusing registrations, and reject
  moved application storage before interpreting overlap checks.
- Open Unix workspace selections as directories so named pipes cannot hold the
  control-store lock indefinitely; test unsupported creation-time metadata.
- Bound workspace CLI test processes, check the exact policy-audit event count,
  and document registry status and internal policy contracts.

- Reject indeterminate ancestor walks and invalid control-file links before
  accepting workspace configuration or selecting legacy session behavior.

- Reserve terminal journal capacity, recover full legacy active turns with one
  bounded terminal tail, and test write-success/sync-failure handling.
- Preflight encoded request events before starting turns, mark deterministic
  reply-size failures as failed turns, and retain locators on chat errors.
- Return lossless native journal locators and support reopening paths that are
  not Unicode; prevent renaming open Windows journals with delete sharing denied.
- Redact the configured local server token from retained/displayed chat text
  and resumed context, and apply a total deadline to each model request.
- Apply title redaction to direct session creation as well as chat, and verify
  redaction of credentials seeded in historical conversation text.
- Add review regression tests and clarify smoke-check, coverage, and CLI examples.

### Added

- Rust 0.9.0 Phase A2c: add trusted `file apply` on Linux ext4 with an exact
  SHA-256/size precondition and an explicit stop-writers acknowledgement. Preserve
  supported owner/group/mode metadata, verify a synced private stage, persist
  commit intent, then attempt one handle-relative rename. Sync the published
  file and both parents before success. Keep unresolved evidence on failure;
  never retry, restore, or delete it. Windows apply remains unavailable.
  Add native publication, fault/reopen, and CLI tests. ADR-0022 records the
  filesystem and metadata limits. Dependency versions are unchanged.

- Rust 0.8.0 Phase A2b: protected Linux checkpoint preparation through the
  authorized core file broker, exact payload/manifest validation, bounded private
  storage, durable initialization and journal appends, and failure/reopen handling.
  Add `file checkpoints` and `file acknowledge-checkpoint` for existing-only
  recovery. Unresolved or invalid state blocks new broker work without growing
  the old operation log. Preserve unused legacy stores and permission changes.
  Windows refuses checkpoint storage before mutation; apply/restore/deletion
  remain disabled. Add native, public-API, CLI, quota, and injected-failure tests.
  Enable the existing Rustix dependency's safe UID-query feature; no dependency
  versions change. ADR-0021 records the storage boundary and remaining gates.

- Rust 0.6.0 read-only replacement-record validation for library callers:
  strict initialization/ledger replay, bounded records and descriptors, native
  path validation, exact manifest references, ordered lifecycle transitions,
  unresolved-state retention, and acknowledgement without replay or success
  synthesis. Add public-API boundary tests and an executable documentation example.
  No filesystem writer, checkpoint store, CLI apply, or broker-admission change
  is enabled. ADR-0019 records the remaining implementation increments.

- Record merged PR #10 as acceptance of ADR-0018 and update the resume baseline.

- Phase A1 replacement/checkpoint/recovery proposal and ADR-0018. Define exact
  preconditions, private same-filesystem staging, bounded checkpoint retention,
  versioned audit, uncertain-outcome handling, and native backend/test gates.
  Update the plan for merged PR #9 and split contract review from Phase A2
  implementation. This documentation enables no overwrite or recovery command.
  Define first-use ledger provisioning and the limit on detecting reverted edits.

- Root implementation plan recording the verified baseline, phase order,
  acceptance criteria, verification commands, and known cautions for resuming.

- Rust 0.5.0 read-only edit preview: `file diff` compares a broker-read
  workspace file with trusted proposed UTF-8 content, returns exact digests and
  a bounded single-hunk diff, and never writes to the workspace or creates a
  checkpoint. Existing-file replacement remains disabled.

- Rust 0.4.0 session-owned turn/tool runtime for library callers and tests:
  strict read/list tool admission, a fresh workspace binding per operation,
  control-store release before model I/O, round/call/byte/deadline limits,
  cooperative cancellation, model-addressable Unicode listing names,
  source-labelled untrusted tool messages, and durable tool intents/results.
  No CLI command or real provider adapter dispatches these tools.
- JSONL session schema version 2 with `tool.requested`, `tool.completed`, tool
  messages, and exact tool schema/name manifest fields. Replay verifies that
  each tool message matches its recorded size and SHA-256. Version 1 journals
  remain readable and writable with their text-only subset. New sessions use
  version 2; mixed versions and version-1 tool events are rejected.

- User-provided pull-request-review skill source package, preserved instructions,
  provenance/digest catalog, and documentation links. Runtime skill loading and
  enablement remain planned.

- OrangeCode application namespace across Rust crates, CLI, token environment
  variable, default data directory, session filenames, repository links, and
  Sonar project configuration. Existing private data is not moved automatically.
- Compiled prompt version 0.1.3 with the renamed identity and an updated digest
  regression check; explicit prototype data-transition instructions.

- Rust 0.3.0 native workspace file broker with bounded reads/listings, exclusive
  new-file/directory creation, current binding checks, and handle-relative
  Linux/Windows enforcement. Existing entries cannot be replaced or deleted.
- Separate bounded operation intent/outcome journal, read withholding on audit
  failure, explicit uncertainty acknowledgement, and no action replay.
- Trusted file CLI commands, native traversal/link/mount and collision tests,
  audit failure/recovery coverage, and an ADR for native file-access boundaries.
  Model tool dispatch and command execution remain disabled. Windows directory
  flag compatibility is an explicit release qualification requirement.

- Rust 0.2.0 workspace settings core: explicit development root, persistent
  directory-backed workspace IDs, permission preferences, revision checks, and
  short-lived process-local session bindings. No file or command tools enabled.
- Bounded single-writer settings/audit JSONL with strict replay, synced
  acknowledgement, poisoned-writer handling, and configuration-only decisions.
- Workspace CLI commands and stable IDs in new sessions when configured, with
  legacy text-only session compatibility and Windows/Linux boundary tests.

- First Rust 0.1.0 CLI prototype, with a pinned toolchain, locked dependencies,
  and native Windows/Linux formatting, lint, and test workflow.
- Compiled base prompt inspection, lower-priority per-request user instructions,
  bounded LM Studio chat, and refusal of all model-requested tools.
- Typed JSONL session creation, request manifests, exclusive writer ownership,
  strict replay validation, archive/restore, and explicit abandoned-turn recovery.
- Native journal-handle checks, prompt/provider/journal/CLI tests, a development
  guide, and an ADR separating this prototype from future controlled execution.
- User-selected project/global memories with review, edit, delete, disable,
  scoped retrieval, provenance, and context/cache invalidation requirements.
- A memory-scope ADR and memory guidance in draft system prompt version 0.1.2.
- A compiled, read-only base prompt contract with separate lower-priority user
  customization and current-task input; draft prompt version 0.1.1.
- MCP client requirements for stdio and Streamable HTTP, scoped server/tool
  permissions, authentication, untrusted context, and supervised execution.
- Session archive/restore and versioned JSONL journals in a UTC date tree, with
  stable filenames, a rebuildable index, crash recovery, and no action replay.
- LM Studio as a separate model connection with explicit endpoint, model,
  authentication, and capability qualification.
- MCP/session acceptance requirements and supporting architecture decisions.
- Product priorities for secure operation, prompt injection defence, and efficiency.
- A visible, versioned SYSTEM prompt draft and scoped custom-instruction design.
- Configurable token-based auto-compaction, safe context budgets, provenance,
  bounded summaries, and recovery requirements.
- Scoped artifact and provider prompt caching, invalidation, clear controls,
  and usage measurements without replaying side effects.
- Skills settings for import, removal, enablement, and disablement, with validated
  packages and the existing controlled execution boundary.
- Acceptance requirements and an ADR for instruction/context trust boundaries.
- Initial project overview, provider plans, and Windows/Linux development roadmap.
- Rust coding rules adapted from Merovingian, with security and testing guidance.
- Required contributor guidance, a documentation index, and platform targets.
- An architecture decision register that records the Rust rule adaptations.
- Required ChatGPT and Claude subscription authentication profiles, with provider
  evidence, explicit billing modes, credential lifecycle, and readiness gates.
- General development-tool access through installed or managed toolchain
  profiles, including scoped runtimes, SDKs, outputs, caches, and subprocesses.
- Authentication and toolchain acceptance requirements and supporting ADRs.
- CodeRabbit automatic reviews using project guidelines, CodeQL language-aware
  analysis, and SonarQube Cloud CI configuration.
- PR-only main protection with no bypass actors and blocks on force-pushes
  and branch deletion.
- Repository-wide CodeRabbit guideline scopes, vendor-aware CodeQL selection,
  and precise analysis coverage and Sonar credential setup guidance.
