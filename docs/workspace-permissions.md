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

## Trusted settings and identity

Only trusted local CLI input can change settings. The commands are not model
tools. Prompts, conversation history, imported IDs, skills, and repository files
cannot set a development root, change permissions, or recreate a binding.

Select an existing development root explicitly. Register an existing workspace
beneath it, including the root itself. Each workspace gets a random UUIDv4.
Read and write preferences default to enabled; process execution and task
network access default to disabled. Provider connections remain separate from
task networking. No operation class currently executes a tool.

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
metadata. Evaluation rejects store, session, workspace, revision, expiry,
availability, and flag mismatches before reporting configuration eligibility.
Checks append audit events without changing the settings revision.

No running tasks exist in this increment. Live task revocation, descendant
cleanup, per-target grants, approval UI, and filesystem enforcement remain
required before tool execution can be enabled.

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
