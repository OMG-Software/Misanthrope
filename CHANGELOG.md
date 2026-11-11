# Changelog

Changes are recorded here before a release version is assigned.

## Unreleased

### Added

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
    stays pending until `runs acknowledge`. `runs audit` lists records, and
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

- The SonarQube Cloud project key is now `OMG-Software_OMGCode`, matching the
  key a Sonar administrator set on the server. `sonar-project.properties` and
  the repository-checks guide use the new key.
- Rename the project and GitHub repository to OMGCode. Version 0.7.0 uses
  `omgcode-*` Rust crates, the `omgcode` command, `OMGCODE_LM_TOKEN`, the OMGCode
  application-data identity, and `omgcode-` session filenames. Prompt version
  0.1.4 carries the new identity. Old names have no runtime aliases. Existing
  private data stays in place; explicit transition instructions are in the
  development guide. Keep the existing Sonar project key to retain its history.

### Fixed

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
