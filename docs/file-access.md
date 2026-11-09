# Controlled workspace file access

## Increment scope

The 0.3.0 broker provides bounded file reads, directory listings, exclusive
new-file creation, and exclusive single-directory creation. Trusted CLI input
exercises these APIs. Chat still advertises no tools and rejects model tool
calls. The process runner, terminal, Git, MCP, and graphical panels are separate
increments. This broker is not process containment or a release qualification.

The CLI session UUID selects an operator trace scope. These manual commands do
not dispatch work from saved conversations. A future conversation runtime must
check archive/lifecycle state before issuing model tool requests; it must not
infer a live binding from journal metadata.

Existing-file replacement, deletion, recursive directory creation, and action
replay are not supported. A create operation refuses every existing entry.
In particular, a hash check followed by rename is not atomic compare-and-swap
against an independent editor. Replacement needs a separate conflict and
recovery contract. See [ADR-0013](adr/0013-enforce-file-access-with-native-handles-and-exclusive-creation.md).

## Authorization

Each operation requires a process-local session binding and explicit request
session/workspace IDs. The broker checks store incarnation, session, workspace,
revision, expiry, current permission preferences, root membership, and private
application-data separation. Reads and listings require read permission;
creation requires write permission. A serialized ID, journal header, summary,
or diagnostic policy decision cannot restore authority.

The core then opens native root/workspace directory handles and compares their
volume, file ID, and creation time with current registrations. Relative paths
are opened through these handles. String-prefix checks derive no authority.
The diagnostic `execution_allowed` field remains false; it is not an executor
permit. The actual broker performs the additional native checks on each call.
Operational checks do not append to the settings journal. Admitted target
operations have their own intent/outcome log, so repeated file requests cannot
consume the capacity needed for settings changes. The explicit `workspace check`
diagnostic still records `policy_checked` in the settings journal.
File admission also stops if settings lack event space or a conservative
permission-change byte budget. A previously full settings log cannot leave
usable file access that the operator cannot revoke.

The control writer remains locked throughout a synchronous bounded operation.
Other OrangeCode settings writers cannot change its policy midway. This does
not isolate the application from hostile code already running as the same
user, mutable filesystem implementations, or a host process moving an opened
directory outside its earlier ancestry. A confined process runner remains a
separate requirement. Do not describe handle checks as whole-user isolation.

## Native paths and objects

Accept at most 4,096 native path units, 32 components, and 255 units per component.
Registration and binding apply the same bounds to the workspace name beneath
the development root. An unusable scope is rejected before file operations.
Invalid target names return a request error; unsupported native controls remain
a separate failure. Names retain Unix bytes or Windows UTF-16 without lossy conversion. Refuse roots,
prefixes, empty components, dot/parent components, and NUL. The empty path denotes
the workspace for listing only. Windows also rejects alternate streams, device
aliases, invalid characters, and trailing-dot/space spellings.

Linux uses `openat2` with `BENEATH`, `NO_SYMLINKS`, `NO_MAGICLINKS`, and `NO_XDEV`
for untrusted traversal. Mount crossings, including bind mounts, are refused.
The required syscall/flags must be available; there is no weaker fallback.
An ordinary file is first pinned with `O_PATH` and checked before opening for
data. The generated `/proc/self/fd/<owned-fd>` path reopens only that retained
regular inode. A second identity check guards this reopen. Procfs availability
is required; caller-supplied paths are never inserted into that generated path.

Windows opens one validated name relative to a retained directory handle with
native no-reparse controls and verifies the opened object's attributes, volume,
type, and link count. Directory enumeration also uses the retained handle.
Unsupported native flags or metadata fail closed. No ambient child-path reopen
or path-based directory enumeration is a fallback.

Windows directory flag compatibility is an explicit release gate. The current
opens and creates combine `FILE_DIRECTORY_FILE` with `FILE_OPEN_REPARSE_POINT`.
[Microsoft's NtCreateFile reference](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile)
omits the latter from its compatible-directory flag list. Passing native tests
shows acceptance on that host; it does not resolve this public-contract gap.
Establish a supported configuration and verify the target Windows version,
filesystem, and filter matrix before release. Do not remove no-reparse controls
or add a weaker fallback to make an incompatible system proceed.

Reads accept regular files with one hard link. Links, reparse points, devices,
and other special files cannot supply file data. Listed names and kinds are
observations only; every later access is reopened and checked. Listings can
reflect concurrent directory changes and are not consistent snapshots.

## Bounds and creation results

| Resource | Bound |
| --- | --- |
| Read or new-file payload | 1 MiB |
| One listing | 512 entries and 64 Ki native name units |
| Operation journal | 8 MiB, 4,096 events, 64 KiB per encoded event |
| Recent audit view | 1–100 entries |

Reads check size and enforce the byte limit during reading. Creation uses
exclusive create semantics, so a collision never truncates an existing file.
Parent directories must already exist. New Unix files request mode 0600 and
new directories mode 0700. Windows inherits the workspace ACL.

A new file is visible while bytes are written. Interrupted writes can leave a
partial new entry. Post-create failure is an uncertain mutation, not a clean
refusal. The broker never automatically deletes that name, because another
actor may have changed it. Inspect the path and operation journal before any
explicit retry. File flushes and journal syncs are used, but cross-platform
power-loss durability, particularly Windows directory entries, remains
unqualified.

## Private operation journal and recovery

`<data-dir>/file-operations-v1.jsonl` records operation intents and outcomes.
It links its header to the existing control store UUID. The settings journal
format stays unchanged, so 0.2.0 settings readers need no migration. Older
binaries provide no file broker and cannot execute operation history.

The file-operation journal uses exclusive writer ownership, strict bounded
JSONL replay, unique IDs, contiguous sequences, UTC timestamps, and synced
acknowledgement. Invalid, empty, linked, incomplete, or foreign-store files
are never reset automatically. File contents are not logged. Relative path names
retain their native form and may themselves contain credentials or other
sensitive data. Protect the journal and audit output as private user data; path
names are not redacted.

An intent is durable before mutation. A read result is released only after its
outcome is durable. Every start reserves two terminal event slots and 4 KiB
for finish and acknowledgement. An audit write/sync failure poisons its writer.
If creation succeeded but its result could not be journaled, the caller receives
an uncertain result with its operation ID.

A complete outcome may still be visible after a sync error. Reopen validates
that record without retrying the operation. Inspect the reported journal state;
an uncertainty returned to the caller does not guarantee an unfinished record.

Replay exposes an unfinished or uncertain operation and starts no work. New
file operations stop until the user explicitly acknowledges that operation
through the trusted operator interface. Acknowledgement records inspection; it
does not assert success, repair a file, retry a write, or grant permissions.
The user must inspect and resolve any partial entry separately. Automatic
repair and a graphical recovery flow are future work.

## Explicit operation-journal archive

When the active log reaches its byte or event cap, use the trusted
`file rotate --archive-dir PATH` command. The directory must already exist and
be private, outside both application storage and the current development root.
It must be on the same filesystem as the active log. This is an explicit export
to operator-managed storage, not a model tool or an automatic retention policy.

The command strictly replays the existing active log and refuses unfinished,
uncertain, invalid, or missing logs. It flushes the file, closes its writer, and
makes one native atomic no-replace move to `file-operations-<UUID>.jsonl`.
It then syncs destination and source directories where supported. The receipt
contains the lossless archive path, exact raw-byte SHA-256, and event count.
No archive is overwritten, deleted, scanned automatically, or replayed as work.

Each export is at most 8 MiB and 4,096 events. Active application storage keeps
its existing bound. Repeated explicit exports consume operator-selected disk
space; OrangeCode does not impose an aggregate quota or manage their retention.
Keep exports private and outside present and future development roots. Move
them elsewhere through the operator's normal file-management process when
needed. Do not assume an export remains private after broadening a development
root to contain it.

The next ordinary operation initializes a fresh active log, preserving the
control store and its current permissions. `file audit --archive PATH` validates
an existing archive for that same store and returns bounded metadata without
appending. Missing archives are not created. Inspection currently requires the
existing read/write exclusive-lock helper, so read-only media are not supported.

A move or directory-sync error can return `file_rotation_uncertain` with the
lossless destination candidate. Inspect both the active path and candidate
before any explicit retry. The command does not reset, retry, or repair either
file. Windows directory power-loss durability remains unqualified.
See [ADR-0015](adr/0015-archive-operation-logs-without-consuming-settings-capacity.md).

See [workspace settings](workspace-permissions.md) for binding/configuration
contracts and [security rules](security-coding-rules.md) for the full boundary.
