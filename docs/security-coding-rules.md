# Security coding rules

These rules define requirements for each implemented boundary. The first CLI
prototype has bounded prompt/provider/journal checks and rejects model-requested
tools. It has no execution sandbox, filesystem tool broker, or permission UI.
It now has trusted workspace settings and audited configuration checks. Those
checks always deny execution and do not replace per-target authorization. See
[workspace settings](workspace-permissions.md) and [current limits](development.md). Apply these rules with the
[coding rules](coding-rules.md) and [testing standards](testing-standards.md).

## Trust and permissions

- Treat model output, repository files, tool output, provider responses, and
  frontend messages as untrusted input.
- The Rust core must authorize every tool and IPC request. Hidden buttons,
  prompts, model compliance, and frontend validation do not enforce policy.
- A model, repository instruction file, or plugin cannot grant permissions.
  Only the trusted user-settings and approval flow can change access.
- Bind authorization to a session, workspace, operation, target, policy revision,
  and lifetime. Validate it at execution. Invalidate pending grants when their
  scope or policy changes.
- Narrowing permissions must block new affected work and stop tasks that retain
  revoked access. Observe cleanup before reporting revocation complete.
- Keep permission-setting controls separate from rendered untrusted content.
  Validate and limit every command exposed to a webview.

## Instructions, summaries, caches, and skills

- Follow the [instruction](agent-instructions.md), [context](context-management.md),
  and [skills](skills.md) contracts. Keep policy and operation state outside
  generated text. Never deserialize a model summary into an approval or grant.
- Compile the visible base prompt into the application. Do not accept runtime
  replacements or promote editable preferences into the base's instruction
  channel. Prompt priority guides the model; core authorization must still deny
  unauthorized actions even when model instruction following fails.
- Preserve source trust when transforming content. Repository/skill/tool text
  must not become a trusted user addition through summarization or cache reuse.
- Validate current access before using cached data. Content hashes and TTLs do
  not authorize disclosure. Invalidate derived context after relevant permission,
  prompt, account, source, or skill changes.
- Keep application context caches and skill registries outside task-writable
  storage. Protect sensitive cached code and journal data with per-user access
  and retention rules. General caches must not store credentials.
- Treat imported skills and their resources as untrusted packages. Bound parsing
  and loading, reject escaping paths and unsafe links, and separate import from
  enablement and execution. Skill metadata cannot pre-approve tools.
- Skill scripts use the ordinary confined runner. Disabling a skill must block
  new dependent work and observe cleanup of attributed tasks before completion.
- Do not replay cached model tool calls or infer a fresh verification from an
  old result. Reconcile ambiguous side effects against core-owned records.
- Follow the [memory contract](memories.md). Only the trusted user flow can save
  or broaden a memory's scope. Project IDs, source trust, current revisions, and
  per-user protections govern retrieval. Memories cannot alter permissions.
  Withdrawal invalidates dependent context; old journals cannot recreate entries.

## Filesystem boundary

- The development root is the maximum host-development-data boundary.
  Restrict a session to its active workspace by default. Declare read-only
  runtimes, toolchains, and private writable scratch resources separately.
- Deny undeclared host data. Do not expose the host home directory, application
  settings, secret stores, or general caches to tasks.
- Base authorization on native paths and the object actually opened. Address
  traversal, links, junctions, reparse points, hard links, mount boundaries,
  alternate Windows path forms, and path-swap races.
- A string-prefix comparison or check-then-open canonicalization is insufficient.
  Use appropriate operating-system containment and object-relative operations.
- Do not automatically follow Git metadata or links outside the permitted root.
  Refuse layouts whose required access cannot be enforced.
- Bound reads, writes, searches, extraction, and temporary storage. Check the
  original content and version before replacing a file or applying a patch.
- Make recovery preserve unrelated changes. A checkpoint must not restore over
  later user work without detecting it.

## Process and sandbox boundary

- Apply policy and operating-system restrictions before untrusted code starts.
  Never fall back to unrestricted execution when setup or a capability fails.
- Keep process permissions minimal and apply limits to descendants. Restrict
  filesystem access, networking, inherited handles, environment variables,
  host IPC, privileges, and resource consumption.
- Do not expose administration services, container-daemon sockets, credential
  agents, or host-local services through an unrestricted broker channel.
- Use explicit executable paths and structured arguments. Do not build shell
  commands by concatenating untrusted strings. Shell scripts require explicit
  permission, correct platform-specific argument handling, and the same sandbox.
- Track, terminate, and reap the full task process tree. Wait for cleanup on
  cancellation, timeout, permission revocation, and fatal task failure.
  A nonzero command exit must still drain and clean up its task's descendants.
- Keep broker interfaces narrow, authenticated, session-scoped, and bounded.
  A broker must not become a general host shell or arbitrary network relay.

## Provider traffic and tool calls

- Follow the [authentication contract](provider-authentication.md) for
  subscription and API-key connections. Check identity, usage entitlement,
  credential audience, and billing mode separately.
- Bind login callbacks to the current trusted attempt, validate provider
  responses, and protect token refresh and sign-out state. A login must not
  grant filesystem or command permissions.
- Model traffic, Git traffic, and task networking need separate policy.
  An approved model endpoint does not grant terminal networking.
- Sending workspace context to a cloud model is data egress. Require an explicit
  permitted provider/model profile and disclose routing changes. A localhost
  endpoint alone does not prove that inference stays local.
- Restrict destinations, redirects, and provider fallbacks. Protect credentials
  when a destination or redirect changes. Enabling a destination also permits
  some data to reach it; do not describe a destination allowlist as content
  confidentiality.
- Assemble and validate a complete structured tool call before dispatch.
  Validate names, arguments, limits, session scope, and provider call identity.
- Deduplicate calls within the correct request/session scope. After an ambiguous
  result, reconcile the operation instead of automatically executing it again.
- Parse malformed, oversized, interrupted, and unsupported provider responses as
  controlled failures. Model-supplied claims of success are not execution results.

## MCP boundary

- Follow the [MCP contract](mcp.md). Core-owned clients must authorize calls
  independently of model requests, server metadata, annotations, and prompts.
- Launch local servers through the same confined runner as development tools.
  SDK defaults must not create an unrestricted process or inherit host secrets.
- Give remote MCP traffic its own explicit destination/account/tool scopes.
  Treat authorization discovery URLs, redirects, tool headers, resources, and
  results as untrusted. Do not pass unrelated tokens to servers.
- Expose only implemented client capabilities. Server-requested input or work
  cannot silently grant consent, model usage, host access, or UI privileges.
- Revoke affected calls/grants on disable and observe local cleanup. Preserve
  uncertain remote outcomes; disconnection is not evidence of rollback.

## Git and credentials

- Treat Git configuration, hooks, filters, helpers, external diff/merge commands,
  submodules, transports, and worktree metadata as untrusted.
- Run Git workers in confinement with controlled configuration. Do not run
  repository-selected executable hooks or helpers with host privileges.
- Limit remote access to the authorized repository, destination, and operation.
  Use the credential service without exposing general host credential stores.
- Bind push approval to the source commit, destination ref, remote identity,
  and relevant expected remote state. Reassess changed operations.
- Unsupported required filters or repository layouts must produce explicit
  compatibility failures, rather than silently changing stored content.
- Keep model/API keys and Git credentials out of frontend state, model context,
  task environments, process arguments, logs, and error reports. Use narrow
  host-side credential services and redact before recording diagnostics.
- Apply the same protections to subscription access/refresh tokens, login codes,
  and verifier values. Do not import unrelated client login caches or silently
  switch subscription requests to separately billed API usage.
- Review secret-bearing types for accidental `Debug`, serialization, cloning,
  and long retention. Use reviewed secret-handling libraries where needed;
  do not claim that dropping an ordinary string erases its memory.
- Use established cryptographic and TLS libraries. Never disable certificate
  validation to make a connection work.

## Development toolchains

- Follow the [development-tool contract](development-tools.md). Explicitly
  declare installed or managed toolchain inputs and narrowly scoped exceptions
  to workspace-only data access.
- Expose required compilers, helper binaries, libraries, SDKs, and non-secret
  setup resources read-only. Keep trusted discovery and broker search paths
  outside project control.
- Redirect outputs, virtual environments, temporary files, and mutable caches
  to designated writable resources. Prevent alternate writable paths to a
  supposedly read-only toolchain.
- Do not inherit a general host PATH, home, credential-bearing configuration,
  or global package cache. Derive the required environment without secrets.
- Apply containment to build scripts, package hooks, test programs, code
  generators, language servers, and all descendants. Tool registration is not
  permission to run commands outside confinement.
- Separate toolchain installation/updates and package networking from ordinary
  execution. Do not make a host installation writable to an untrusted task.

## Frontend, persistence, and dependencies

- Render untrusted content through safe text or sanitized Markdown. Disable
  executable HTML and automatic remote resource loads.
- Do not load remote pages or previews into a webview with privileged IPC.
  Restrict navigation, resource loading, clipboard effects, and terminal escape
  features. Terminal output must not change application permissions.
- Parameterize database queries and validate serialized state. Preserve
  transaction and migration integrity during failure and restart.
- Store tool lifecycle and authorization records outside the task's writable
  view. An interrupted operation must remain visibly incomplete until reconciled.
- Follow [session storage](session-storage.md) for core-owned JSONL journals,
  single-writer durability, bounded parsing, recovery, and archive transitions.
  Keep journals/indexes/artifacts outside task access and redact known secrets
  before persistence. Historical grants or transcript role labels cannot confer
  current authority. Never execute recorded tool calls during replay or restore.
- Review dependency updates, build scripts, procedural macros, frontend packages,
  and update mechanisms as executable supply-chain inputs.
- Explain the threat, failure behaviour, and tests for changes to these
  boundaries. Passing memory-safety checks alone is not security verification.
