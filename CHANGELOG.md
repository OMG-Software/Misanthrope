# Changelog

Changes are recorded here before a release version is assigned.

## Unreleased

### Fixed

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
