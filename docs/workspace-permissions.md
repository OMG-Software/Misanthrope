# Workspace identity and permission settings

## Implemented boundary

The 0.2.0 CLI stores a user-selected development root, stable workspace IDs,
permission preferences, a global settings revision, and audit events. This is
configuration infrastructure. It does not implement a file broker, runner,
permission settings page, Git tools, or process isolation.

Every diagnostic result has `execution_allowed: false`. A result with
`configured_allowed: true` means the current preferences allow that operation
class for that binding. It is not a grant to open a file or start a process.
Future tools must check the target actually opened and apply native controls.
Never pass these results to an unrestricted executor.

Since 0.16.0 the live `execute` preference gates the trusted CLI command
`omgcode run`. The command checks the preference itself on every run and
starts the program only under native confinement. It does not consume a
diagnostic result; see [trusted runs](execution.md#trusted-runs).

## Trusted settings and identity

Only trusted local CLI input can change settings. The commands are not model
tools. Prompts, conversation history, imported IDs, skills, and repository files
cannot set a development root, change permissions, or recreate a binding.

Select an existing development root explicitly. Register an existing workspace
beneath it, including the root itself. Each workspace gets a random UUIDv4.
Read and write preferences default to enabled; process execution, task
network access, host services, and model tools default to disabled. Provider
connections remain separate from task networking. No operation class executes
a tool by itself. The separate `model_tools` and `host_services` preferences
are described under [model tools](#model-readlist-and-command-tools) and
[host services](#host-services).

Registration stores the canonical native path, volume/filesystem ID, 128-bit
file ID, and directory creation timestamp. Canonicalization resolves a trusted
configuration selection; it does not authorize later I/O. Native path units are
preserved without Unicode replacement. Ancestor checks compare live directory
identities, with a maximum of 256 ancestors. Exhausting the bound is an error,
never proof that two directories are separate. They do not use string prefixes.
Directory creation-time support is required; unsupported filesystems fail.
After resolving a trusted selection, Unix opens the canonical path with
`O_DIRECTORY | O_NOFOLLOW`. A FIFO or a newly introduced final-component link
then fails at open. This does not protect parent-directory races.

Registering the same directory again reuses its ID without a revision change.
Aliases of that directory can reuse the same ID. A replacement at a registered
path cannot inherit the old ID or preferences. If a saved workspace path is
missing or now names a different directory, binding or registration fails with
`DirectoryChanged`. Renaming or moving a workspace does not migrate its
registration. While its saved path is missing or stale, registration at a new
path cannot reuse the old ID or preferences. Bindings recheck workspace identity and root
membership. The application data directory and development root must not
overlap in either direction; the saved application-data directory identity is
checked before the overlap check. Stale data-directory identity fails closed.

These metadata checks detect ordinary replacement and configuration mistakes.
They do not prevent path-swap races, hostile mount changes, file ID reuse with
forged metadata, or a malicious process running as the same user. File access
will need a reviewed object-relative broker. No such broker exists yet.

## Revisions and bindings

Every mutation requires the expected global revision. A stale value fails
without changing state. Root changes, new registrations, and changed flags
increment the revision. Idempotent settings operations do not append an event.
Changing the root preserves saved registrations, but unavailable or out-of-root
workspaces cannot obtain a new binding. There is no automatic reassignment or
path migration. A renamed workspace with a missing saved path cannot be
re-registered at its new path by reusing the old record; an explicit future
migration flow would need to define that operation.

A process-local session binding identifies the control store, session,
workspace, and revision. It expires after 15 minutes on a monotonic clock. It
cannot be cloned or deserialized. Every configuration change makes all earlier
bindings stale. The core turn runtime opens the control store, obtains a fresh
binding, and releases the settings lock for every tool operation instead of
retaining one across model rounds; it never reconstructs a binding from journal
metadata. The read-only `file diff` preview also requires the read preference.
It is not a write grant. Trusted Linux ext4 apply separately checks live read
and write preferences and the native replacement requirements. Evaluation
rejects store, session, workspace, revision, expiry,
availability, and flag mismatches before reporting configuration eligibility.
Checks append audit events without changing the settings revision.

Trusted CLI runs exist since 0.16.0. A run needs the live `execute`, `read`,
and `write` preferences, `host_services` when it selects host services, and,
since 0.18.0, `task_network` when it selects network access with
`--network`.
It checks them before it starts and again after every settings change while
it runs. A change that withdraws one stops the running tree, which is recorded
as `revoked`. The `permissions` command reports success only after the runs it
affects have stopped; see [settings changes during a
run](execution.md#settings-changes-during-a-run). Per-target grants and
approval UI remain required before a model can request execution.

## Persistence and audit

The source of truth is `<data-dir>/control-v1.jsonl`, outside the development
root. It combines settings changes and audit records in one append-only log.
This avoids a separate settings write succeeding without its audit event.
The process holds an exclusive writer lock only for the local operation and
releases it before a model request. It never holds a session lock while opening
the control store. Each acknowledged event is synced before state is published.

Every envelope has schema version 1, store/event UUIDv4 values, a contiguous
zero-based sequence, UTC timestamp, global revision, and tagged `action`:

| Action | Meaning |
| --- | --- |
| `created` | Initial store identity, sequence and revision zero |
| `root_set` | Selected directory identity; increment revision |
| `workspace_registered` | New ID, directory identity, default preferences; increment revision |
| `permissions_changed` | Existing ID and complete preference set; increment revision |
| `policy_checked` | Explicit diagnostic binding/request check and decision; preserve revision |
| `profile_added` | New toolchain profile; increment revision |
| `profile_removed` | Remove a toolchain profile; increment revision |
| `package_profile_added` | New package-network profile; increment revision |
| `package_profile_removed` | Remove a package-network profile; increment revision |
| `package_fetch_started` | Trusted package-fetch intent; preserve revision |
| `package_fetch_finished` | Trusted package-fetch outcome; preserve revision |
| `package_cache_root_set` | Select one workspace's package cache root; increment revision |
| `package_cache_root_cleared` | Clear one workspace's package cache root; increment revision |
| `provider_profile_added` | New provider profile; increment revision |
| `provider_profile_removed` | Remove a provider profile; increment revision |
| `approval_requested` | Trusted approval request; preserve revision |
| `approval_decided` | Approval granted or denied; preserve revision |
| `approval_consumed` | Approval used once; preserve revision |

Operational file checks do not append `policy_checked`. They use current
settings and their separate [operation journal](file-access.md). This prevents
file traffic or a full operation log from consuming settings capacity.
Workspace registration and binding reject relative scopes beyond the broker's
name/depth bounds, with a workspace-specific error.

Limits are 8 MiB per journal, 4,096 events, 64 KiB per encoded event including
the newline, and 256 registered workspaces. Audit output returns 1 to 100 recent
events. Native paths have at most 32,768 units. Audit rows contain identifiers
and policy metadata, never prompts or dedicated credential fields. Native paths
can themselves contain secrets. Treat journal records and audit output as
private data.

Opening validates the complete bounded log. Empty files, incomplete tails,
duplicate JSON keys or event IDs, unknown fields/schema/events, invalid
transitions, and sequence/revision inconsistencies fail without repair. Only
an absent file permits initialization. Existing links, including dangling links,
are invalid state; they cannot select the legacy no-settings path. Write or sync failure poisons that
writer; no decision is returned if its audit append fails. A complete record
can still exist after an uncertain sync, so reopen validates without retrying
work. There is no rotation, automatic repair, deletion, or directory-rebind
command yet. Capacity exhaustion stops changes and checks; it never drops audit
events. Keep the original log for future reviewed migration/recovery.

New Unix directories request mode 0700 and files 0600. Existing permissions are
not changed. Windows inherits ACLs; explicit per-user ACL setup and power-loss
durability remain unqualified. Parent paths must be trusted. Native file checks
and locks do not protect against a hostile same-user application.

## Container grants

On Windows, `workspace container-grant` and `workspace container-revoke` give
and remove ACL grants for a workspace's AppContainer. A grant needs the expected
revision and increments it; revocation needs none and keeps it. `workspace
status` lists outstanding grants under `container_grants` and omits the field
when there are none, so settings without grants keep their exact output.
Version 0.14.0 refuses a settings log that records a grant. The
[execution guide](execution.md#container-grants) owns the rules.

## Toolchain profiles

`workspace profile-add`, `profile-show`, and `profile-remove` manage the
operator-declared toolchain profiles that `omgcode run` uses. Adding and
removing need the expected revision and increment it, through the
`profile_added` and `profile_removed` events. `workspace status` lists
profiles under `profiles` and omits the field until a profile exists. Each
permission view now also includes `host_services`, so the output is not the
same as version 0.15.0's. Version 0.15.0 refuses a settings log
that records a profile. The
[execution guide](execution.md#toolchain-profiles) owns the rules. Profile
variables are stored here in plain text; never put secrets in them.

## Package-network profiles

`workspace package-profile-add`, `package-profile-show`, and
`package-profile-remove` record operator-declared registry origins as
configuration only. Adding and removing need the expected revision and
increment it, through the `package_profile_added` and
`package_profile_removed` events. `workspace status` lists profiles under
`package_profiles` and omits the field until a profile exists. Each profile
has a name and 1 to 8 origins; there are at most 32 profiles, and one origin
text is at most 2,048 bytes. No run accepts a profile, and a profile never
authorizes network access. Version 0.19.0 refuses a settings log that records
one; a 0.20.0 control journal cannot be read by 0.19.0. The
[execution guide](execution.md#package-network-profile-contract-only) owns the
contract and the remaining enforcement gate.

A trusted `packages fetch` appends `package_fetch_started` and
`package_fetch_finished` events. They preserve the settings revision. They
record the profile identity and revision, a SHA-256 of the request URL (never
the URL text), the cache name, limits, HTTP status, byte count, and content
digest. A failed fetch records a controlled reason; an abandoned fetch leaves
its start event. A control journal written by 0.21.0 cannot be read by 0.20.0.

## Package cache roots

`workspace set-package-cache <workspace-id> <dir> --revision <n>` selects one
workspace's fetched-package cache directory. It must exist, must not overlap
application storage or the development root, and must not overlap another
workspace's cache root. `workspace clear-package-cache <workspace-id>
--revision <n>` removes the selection and leaves cached files in place. Both
use the expected revision and increment it through `package_cache_root_set`
and `package_cache_root_cleared`. `workspace status` lists selected roots under
`package_cache_roots` and omits the field until one exists. A control journal
written by 0.23.0 cannot be read by 0.22.0. The
[execution guide](execution.md#trusted-package-fetch) owns the fetch, exposure,
and grant rules.

## Provider profiles

`workspace provider-profile-add <name> --kind <kind> --auth <auth>
--revision <n>`, `provider-profile-show <name>`, and
`provider-profile-remove <name> --revision <n>` record provider connection
shapes. They store no credential, endpoint, or account. Adding and removing
use the expected revision and increment it through `provider_profile_added`
and `provider_profile_removed`; `workspace status` lists them under
`provider_profiles` and omits the field until one exists. The
[provider authentication guide](provider-authentication.md) owns the kind and
authentication matrix. A control journal written by 0.24.0 cannot be read by
0.23.0.

## Approval records

`approvals request`, `list`, `show`, `approve`, `deny`, and `consume` manage
trusted per-call approvals. A request records a session, workspace, operation,
exact request digest, summary, settings revision, and expiry. The events
`approval_requested`, `approval_decided`, and `approval_consumed` preserve the
settings revision. A decision or consumption is refused after expiry,
mismatch, reuse, or any settings change. The `workspace_run_command` model tool
consumes an approval; the CLI consume command marks a record used but executes
nothing. The
[agent instructions guide](agent-instructions.md#approval-records) owns the
tool-side contract. See
[ADR-0039](adr/0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
and [ADR-0040](adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).

## Task network

`workspace permissions <id> --revision <n> --task-network true` lets a
trusted `run` of that workspace select outbound IP networking when the run
also passes `--network`. Both are required; the preference alone changes
nothing. It is off by default. Turning it off stops networked runs of the
workspace. Model connections never use it. A networked Linux run can reach
every destination, including loopback and the local network; see
[network access](execution.md#network-access).

## Host services

`workspace permissions <id> --revision <n> --host-services true` lets a
trusted Linux `run` of that workspace select host-service Unix-socket access
when the run also passes `--host-services`. Both are required. The preference
is off by default and appends `permissions_changed` like the other flags. It is
not an operation class, so `workspace check` does not mention it. Windows
refuses host services at run time whatever the preference says.

`workspace status` always reports `host_services`. Persisted settings omit the
field while it is `false`, so settings that never enable it keep their exact
bytes. Once a `true` value is written, version 0.15.0 refuses the settings
log. Host-service access exposes every Unix socket the user can reach,
including the Docker daemon; see
[Unix-domain sockets](execution.md#unix-domain-sockets).

## Model read/list and command tools

`workspace permissions <id> --revision <n> --model-tools true` lets `chat` offer
the local model two tools for that workspace: `workspace_read_file` and
`workspace_list_directory`. The preference is off by default, and there is no
per-run flag or per-call confirmation. It is a trusted settings change like the
other flags. It needs the expected revision, appends `permissions_changed`, and
increments the revision. It is not an operation class, so `workspace check` and
policy audit records do not mention it.

When `model_tools`, `execute`, `read`, and `write` are all enabled, the runtime
also offers `workspace_run_command`. Its first call records a pending approval
and does not run; after `approvals approve <id>`, a second identical call
consumes the approval and starts the trusted run. The model cannot approve.
The tool uses fixed limits and no network or host services. See
[model command tool](execution.md#model-command-tool) and
[ADR-0040](adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).
The tool schema version is 0.2.0.

`workspace status` always reports `model_tools`. Persisted settings omit the
field while it is `false`, so settings that never enable it keep their exact
bytes and remain readable by version 0.11.0. Once a `true` value is written,
version 0.11.0 refuses the settings log.

Before each turn, `chat` reads current settings and offers the tools only when:

- the build runs on Linux or Windows;
- a control store exists;
- the session's recorded workspace UUID matches a registered workspace;
- that workspace's `model_tools` preference is enabled;
- the session journal uses schema version 2.

Otherwise chat stays text only and reports `model_tools.mode` as `disabled` with
a `reason`: `platform_unsupported`, `settings_absent`,
`workspace_unregistered`, `preference_disabled`, or `session_schema_v1`. Invalid settings fail with a
`control` error before any model request. The recorded UUID only selects a
registration. The workspace ID and preference used come from current settings.

Before every call, the core runtime opens the settings again and creates a
fresh binding. It denies the call with `model_tools_disabled` unless the
preference is still enabled. The broker then requires the live `read`
preference and denies the call with `permission_denied` otherwise. A denial
ends the turn. A change made during model I/O applies to the next call.

Earlier tool results stay in the journal. A turn sends them to the model again
only while the live `model_tools` and `read` preferences both allow it, checked
when the turn starts. Otherwise, including every text-only turn, each is
replaced by a fixed withheld notice and the output reports
`withheld_tool_results`. Revoking either preference therefore stops further
disclosure of earlier reads; it cannot recall what the model already received.

A model call never reads outside the session workspace, which is registered
beneath the development root. Parsing rejects absolute, drive-qualified, root,
current-directory, and parent paths before dispatch. The broker refuses links,
junctions, hard links, and names outside the workspace. Results are
source-labelled, untrusted user content; the compiled base prompt stays the
only system message. Limits are four model rounds, eight calls per round,
16 calls per turn, and 256 KiB of tool output per turn.

The journal selects the workspace. A crafted journal can therefore select
another registered workspace that has `model_tools` enabled. Calls remain inside
that workspace and need its live preferences, and the output names the workspace
ID used. See [ADR-0026](adr/0026-offer-read-list-tools-through-a-workspace-preference.md).

## Session compatibility

With a configured root, new CLI sessions register their selected workspace and
record its stable ID. Invalid settings or an out-of-root selection fail; there
is no fallback to a fresh legacy ID. The control lock is released before the
session journal is created or a model request starts. If session creation then
fails, its registration can remain as a valid independent settings change.

Without a configured root, text-only chat retains the earlier per-session
association ID. This compatibility path creates no development root or access
grant. Existing journals are not rewritten. Resuming a saved text conversation
does not infer workspace authority from its header. Policy bindings are never
restored from JSONL. Session storage and archive rules remain in the
[session contract](session-storage.md).

See the [development guide](development.md) for CLI examples and
[ADR-0012](adr/0012-separate-workspace-settings-from-execution-authority.md) for
the design rationale.
