# Changelog

Changes are recorded here before a release version is assigned.

## Unreleased

### Added

- Version 0.10.0 enables checkpoint storage and trusted `file apply` on Windows
  NTFS. Apply preserves the owner, group, DACL, and content-indexing opt-out, and
  refuses metadata it cannot reproduce. The stage stays owner-only until the
  commit attempt, and a failure before the rename tries to restore that DACL.
  A crash or failed restore in that window can leave a retained stage with the
  source DACL, which may expose unpublished bytes to principals it admits.
  Windows makes no power-loss claim and is not qualified for release; see
  ADR-0024.
- Add a Windows NTFS private-store backend with exact protected per-user DACLs,
  handle-relative access, and a failing directory flush. Microsoft documents no
  durability contract for the flush; see ADR-0023.

### Changed

- Rename the project and GitHub repository to OMGCode. Version 0.7.0 uses
  `omgcode-*` Rust crates, the `omgcode` command, `OMGCODE_LM_TOKEN`, the OMGCode
  application-data identity, and `omgcode-` session filenames. Prompt version
  0.1.4 carries the new identity. Old names have no runtime aliases. Existing
  private data stays in place; explicit transition instructions are in the
  development guide. Keep the existing Sonar project key to retain its history.

### Fixed

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
