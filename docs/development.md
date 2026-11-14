# Build and run the core prototype

OMGCode 0.25.0 is a developer CLI prototype with workspace permission
settings and bounded native file operations through trusted operator commands.
It can send a text conversation to an explicitly selected local LM Studio model
and save that conversation. It can read/list files, exclusively create new
files/directories, and preview a bounded diff against trusted proposed content
in a registered workspace. When a workspace enables `model_tools`, chat runs
the core's bounded turn/tool runtime so the model can read and list files in
that workspace; with `execute`, `read`, and `write` enabled it can request an
approval-gated command. Trusted CLI apply can
replace an existing file on Linux ext4 or Windows NTFS. Trusted restore puts a
retained checkpoint's original bytes back through apply, and trusted checkpoint
deletion removes one resolved private entry. The trusted `run` command starts
one program in a workspace whose `execute` preference is enabled, under
Landlock and a seccomp socket filter on Linux or a per-workspace AppContainer
on Windows. It uses an operator-declared toolchain profile and records each
run in a run journal. Every run is limited in memory, live processes, and CPU
time. A program started by `run` can create, change, and
delete workspace files without a checkpoint. The CLI's `file` commands cannot
delete workspace files, and the CLI cannot use Git or connect to MCP servers.
When a workspace enables `model_tools`, chat lets the local model read and list
files in that workspace and, with execute/read/write, request an approval-gated
command. Model write, network, Git, and MCP tools are
not implemented. Trusted commands record and apply the Windows container
grants; see [command execution](execution.md).

The core's byte-input replacement inspectors remain read-only. Its file broker
also provides `checkpoint_prepare` on Linux and Windows with fresh read/write
authority.
Private initialization, exact manifest/payload validation, bounded retention,
and synced journal appends are implemented. No CLI command creates a standalone
checkpoint. The CLI provides existing-only inspection and acknowledgement:

```text
omgcode --data-dir /private/omgcode-data file checkpoints
omgcode --data-dir /private/omgcode-data file acknowledge-checkpoint OPERATION_UUID
```

These commands require existing workspace settings, but no workspace/session
selection. Inspection never initializes storage and returns metadata only.
Acknowledgement never applies, restores, deletes, retries, or grants access.
A standalone prepared checkpoint remains unresolved until acknowledged.
On Windows the data folder must grant access only to the current user, SYSTEM,
and Administrators, including through inherited grants. Otherwise checkpoint
storage is refused; choose a private `--data-dir` or remove the extra grant.
See the
[checkpoint storage contract](file-access.md#implemented-checkpoint-storage-phase-a2b).

For a trusted apply, stop all independent writers before starting. Use the
preview's current SHA-256 and exact byte count as the precondition. Supply the
complete proposed content from a local file:

```text
omgcode --data-dir /private/omgcode-data file apply WORKSPACE_UUID notes.txt --session SESSION_UUID --content-file /private/proposed.txt --expected-sha256 CURRENT_SHA256 --expected-bytes CURRENT_BYTES --writers-stopped
```

The flag confirms an operating assumption. It does not lock external editors
or make the rename conditional on the expected bytes. A reported uncertainty
requires inspection of the target and retained checkpoint evidence before
acknowledgement or any new apply. See the
[native apply contract](file-access.md#implemented-native-apply-phase-a2c).

To restore a retained checkpoint, read the file's current SHA-256 and byte count
first. Those values describe the current file, not the checkpoint. The restore
checkpoints the current bytes as a new operation before it publishes:

```text
omgcode --data-dir /private/omgcode-data file restore WORKSPACE_UUID notes.txt --session SESSION_UUID --checkpoint OPERATION_UUID --expected-sha256 CURRENT_SHA256 --expected-bytes CURRENT_BYTES --writers-stopped
```

A checkpoint restores only the workspace and exact path it recorded. A later
edit causes `checkpoint_conflict` without mutation. To free checkpoint capacity,
acknowledge any unresolved operation, then permanently delete one entry:

```text
omgcode --data-dir /private/omgcode-data file delete-checkpoint OPERATION_UUID --permanent
```

Deletion removes the entry's payloads, manifest, and any retained stage. The
ledger keeps its history. If deletion is interrupted, every file operation is
refused until the same command is repeated. See the
[restore and deletion contract](file-access.md#implemented-restore-and-checkpoint-deletion-phase-a4).

## Build

Install Rust through rustup and use a native Windows or Linux development
environment. Windows needs the MSVC linker and Windows SDK, normally supplied by
Visual Studio Build Tools with Desktop development with C++. Linux needs a
working C linker. Run these commands from the repository root:

```text
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --workspace --locked
cargo run --locked -p omgcode-cli -- capabilities
cargo run --locked -p omgcode-cli -- prompt
```

The repository pins Rust 1.99.1, edition 2024, for all three crates. This is also
the declared minimum version; no older compiler is supported. Cargo.lock fixes
the resolved dependency graph. The executable is named omgcode.

## Application namespace and existing prototype data

The command is `omgcode`, the model-token variable is `OMGCODE_LM_TOKEN`,
and new session filenames start with `omgcode-`. Default private storage uses
the OMGCode application identity; Linux normalizes its directory name to
`omgcode`. The application does not search for or move previous data.

Version 0.7.0 replaces the OrangeCode namespace. Update scripts and Rust
dependencies to the new command and crate names. Set `OMGCODE_LM_TOKEN` in
trusted launch configuration if authentication is required. The earlier
`ORANGECODE_LM_TOKEN` name is no longer read. Existing private data remains in
its original location; the new default does not import earlier settings.

To keep existing workspace settings, select their original private directory
explicitly with `--data-dir`. Do not relocate the control journal: its recorded
directory path and native identity must still match. Configure a new store
through the trusted workspace commands if the old directory must move.

To reuse a session with a different filename prefix, first close all writers
and keep a private backup. Change only the filename prefix to `omgcode-`;
retain the timestamp, UUID, `.jsonl` extension, and all journal bytes. Then use
`session inspect` with its explicit path and the intended `--data-dir` before
continuing. The JSONL schemas are unchanged. Inspection does not grant workspace
access or replay actions. For an earlier `orangecode-` filename, replace only
that prefix with `omgcode-`. See
[ADR-0020](adr/0020-rename-the-project-to-omgcode.md).

## Workspace settings

Use a private application data directory outside your development root. These
examples use Windows paths; on Linux use separate paths such as
`/home/me/.local/share/omgcode` and `/home/me/dev`. Create the root and
project directories first. Each command returns JSON. Use the current revision
from its output for the next mutation; replace WORKSPACE_ID and SESSION_UUID
with returned workspace ID and a UUIDv4 session selection.

```text
omgcode --data-dir C:\omgcode-data workspace status
omgcode --data-dir C:\omgcode-data workspace set-root C:\dev --revision 0
omgcode --data-dir C:\omgcode-data workspace register C:\dev\project --revision 1
omgcode --data-dir C:\omgcode-data workspace permissions WORKSPACE_ID --revision 2 --write false
omgcode --data-dir C:\omgcode-data workspace permissions WORKSPACE_ID --revision 3 --model-tools true
omgcode --data-dir C:\omgcode-data workspace check WORKSPACE_ID --session SESSION_UUID --operation read
omgcode --data-dir C:\omgcode-data workspace audit --limit 20
omgcode --data-dir C:\omgcode-data workspace container-grant WORKSPACE_ID C:\dev\project --access read-write --revision 4
omgcode --data-dir C:\omgcode-data workspace container-grant WORKSPACE_ID C:\Users\me\.cargo --access read-execute --revision 5
omgcode --data-dir C:\omgcode-data workspace container-revoke GRANT_UUID
```

`status` initializes an absent store at revision zero. `check` and `audit`
require existing state. Flags take explicit `true` or `false` values. Read/write
default to enabled; execute, task-network, host-services, and model-tools
default to disabled. All decisions still report `execution_allowed: false`;
only the trusted `run` command starts a process, and the model command tool
reaches it only after consuming an approval. Settings commands do not dispatch
tools. `--model-tools true` lets chat offer the model read/list tools and, with
execute/read/write, the approval-gated command tool; see
[model tools](workspace-permissions.md#model-readlist-and-command-tools).
See the [workspace contract](workspace-permissions.md) for limits and failures.
`container-grant` and `container-revoke` work on Windows only. They change ACLs
for the workspace's AppContainer and record each change; see
[container grants](execution.md#container-grants).

## Trusted runs

Enable `execute`, declare a toolchain profile, and run a program. On Windows,
grant the workspace read-write first, and grant read-execute on any toolchain
root that application packages cannot already read. These examples continue
the revisions above.

```text
omgcode --data-dir C:\omgcode-data workspace permissions WORKSPACE_ID --revision 6 --execute true
omgcode --data-dir C:\omgcode-data workspace container-grant WORKSPACE_ID C:\Users\me\.rustup\toolchains\1.99.1-x86_64-pc-windows-msvc --access read-execute --revision 7
omgcode --data-dir C:\omgcode-data workspace profile-add rust --root C:\Users\me\.rustup\toolchains\1.99.1-x86_64-pc-windows-msvc --path C:\Users\me\.rustup\toolchains\1.99.1-x86_64-pc-windows-msvc\bin --root C:\Windows\System32 --path C:\Windows\System32 --revision 8
omgcode --data-dir C:\omgcode-data run WORKSPACE_ID --profile rust -- cargo build --offline
omgcode --data-dir C:\omgcode-data runs audit --limit 20
omgcode --data-dir C:\omgcode-data runs acknowledge RUN_UUID
omgcode --data-dir C:\omgcode-data runs archive --archive-dir C:\omgcode-archive
```

A Windows profile that links with MSVC also needs the MSVC tools and Windows
Kits directories as roots, the MSVC `bin\Hostx64\x64` directory on `PATH`, and
the `INCLUDE`, `LIB`, and `VCINSTALLDIR` values a developer prompt would set.
On Linux a typical profile is `--root /usr --path /usr/bin` plus the
toolchain's own root. The run receives only the profile's `PATH` and
variables, plus runner-owned home and temporary locations. Output is printed,
never stored. `--host-services` also needs `--host-services true` on the
workspace and works only on Linux. `--max-memory-mib`, `--max-processes`,
`--max-cpu-secs`, and `--max-disk-mib` set the
[tree limits](execution.md#tree-limits); the defaults are 8192 MiB, 1024, 7200
seconds, and 8192 MiB of net disk growth. The disk limit is sampled, not a
filesystem quota. The [execution guide](execution.md#trusted-runs)
owns the checks, the journal, and the known limits, including those for Node
on Windows.

On Linux, start `run` in a delegated cgroup of its own, or it is refused:

```text
systemd-run --user --scope -p Delegate=yes -- omgcode --data-dir ~/omgcode-data run <workspace-id> --profile system -- cargo test
```

The scope must hold only that command, so do not start a shell inside it and
run several commands there.

When a root is configured, new sessions use stable IDs from the persisted
workspace registration. Invalid settings or outside-root selections fail.
Existing text-only journals remain readable without inferred access grants.
With no configured root, the earlier text-only behavior remains available and
sets no implicit root. Registrations store settings and audit history; they do
not create project folders or grant access. A broader project registry and the
folder/session tree remain future work.

## Workspace files

Scoped file commands (`read`, `list`, `create`, `mkdir`, `diff`, `apply`, and
`restore`) require
existing workspace settings, a configured development root, a registered
workspace ID, and an explicit session UUID. They do not initialize settings,
infer a root, or treat a journal's workspace metadata as a grant. `read`, `list`,
and `diff` require the read preference. `create` and `mkdir` require the write
preference. `apply` and `restore` require both preferences and a supported
native mount. Execute remains disabled. The `audit`, `acknowledge`,
`checkpoints`, `acknowledge-checkpoint`, and `delete-checkpoint` commands require
existing workspace settings.
They do not require a configured root, workspace registration, or session
selection. Chat advertises the read/list tools when the session's workspace
enables `model_tools`, and the command tool when `execute`, `read`, and `write`
are also enabled; see
[model tools](workspace-permissions.md#model-readlist-and-command-tools).

Capability JSON separates implementation from availability. On Linux and
Windows, `workspace_file_broker_implemented` is `true`, while
`workspace_file_broker` and `exclusive_file_creation` are `null` (unknown). On
other platforms, all three values are `false`. The CLI does not probe the ambient
filesystem for capability reporting. A command checks settings, scope, policy,
and native requirements when it runs. These values do not promise that an
operation can proceed. `edit_previews` is `true` on Linux and Windows for the
trusted CLI preview. `existing_file_replacement` is `true` on Linux and Windows to indicate
implementation, and `false` elsewhere. The CLI does not probe the mount, metadata,
or permissions for that flag. `checkpoint_restore` and `checkpoint_deletion`
follow the same rule for `file restore` and `file delete-checkpoint`.
`model_read_list_tools` is `true` on Linux and Windows, and `tool_policy` is
`read_list_only_when_workspace_model_tools_enabled` there. Neither reports
whether any workspace enables the preference. `workspace_file_tools`, which
covers model write or general file tools, remains `false`.

```text
omgcode --data-dir C:\omgcode-data file read WORKSPACE_ID --session SESSION_UUID src\main.rs --max-bytes 65536
omgcode --data-dir C:\omgcode-data file list WORKSPACE_ID --session SESSION_UUID src --limit 100
omgcode --data-dir C:\omgcode-data file create WORKSPACE_ID --session SESSION_UUID notes.txt --content "Review this file"
omgcode --data-dir C:\omgcode-data file mkdir WORKSPACE_ID --session SESSION_UUID notes
omgcode --data-dir C:\omgcode-data file diff WORKSPACE_ID --session SESSION_UUID src\main.rs --content-file C:\drafts\main.rs
omgcode --data-dir C:\omgcode-data file audit --limit 20
omgcode --data-dir C:\omgcode-data file acknowledge OPERATION_UUID
omgcode --data-dir C:\omgcode-data file rotate --archive-dir C:\private-audit-exports
omgcode --data-dir C:\omgcode-data file audit --archive ARCHIVE_PATH --limit 20
```

Omit the list path to list the selected workspace root. The empty path is not
valid for read, create, mkdir, or diff. Reads return bounded bytes as untrusted
data. Creation refuses every existing entry. `file diff` reads the current file
through the broker, reads the complete proposed content from the trusted
operator-selected local file, and returns a bounded single-hunk diff with exact
digests and sizes. The proposed source is opened read-only without following a
final link; Windows reparse, device, and non-regular handles are rejected. It
accepts only UTF-8 content, limits each side to 1 MiB and, when a diff is
needed, 65,536 lines, and limits the rendered diff to 64 KiB. It never writes to the workspace, creates no
checkpoint, and is not a replacement operation. The preview is a snapshot of the
bytes read; a later replacement must recheck the content identity. File-operation intents and outcomes use
a separate private `file-operations-v1.jsonl`; the `control-v1` settings journal
and session journal schema remain unchanged. The file journal stores operation
metadata, not file contents. If a create result is uncertain, inspect the path
and audit record, then acknowledge the operation. Acknowledgement records
inspection only. It does not retry, repair, or delete the entry. See the
[workspace file-access contract](file-access.md) for bounds and native failures.

The archive directory must already exist, be private, be on the same filesystem,
and be outside both the development root and application-data directory.
`file rotate` and archived audit inspection need existing settings, but no
workspace/session selection. Rotation refuses unresolved operations and returns
a lossless path, SHA-256, and event count. It preserves the original bytes and
does not delete or replay archives. The next file operation starts a fresh log.
A `file_rotation_uncertain` error requires inspection of the active path and the
reported candidate before retry. Export retention is managed by the operator.
See [archive and recovery details](file-access.md#explicit-operation-journal-archive).

## Core turn/tool lifecycle

`omgcode_core::turn` provides a session-owned runtime for one bounded
model/tool turn. `chat` runs it when the session's workspace enables
`model_tools`. A caller supplies a model implementation, an open schema-version-2 session, the
trusted application-data directory, and the workspace ID. The runtime:

- parses strict OpenAI-style function calls for `workspace_read_file` and
  `workspace_list_directory`;
- rejects unknown tools, malformed or duplicate arguments, absolute or parent
  paths, and duplicate call identifiers before dispatch;
- opens the control store for each broker call, obtains a fresh session binding,
  and releases the settings lock before the next model request;
- reserves assembled-prompt capacity and enforces the selected per-message limit
  before recording any tool intent, result, or message;
- enforces hard maxima of four rounds, eight calls per round, sixteen calls per
  turn, 16 KiB per source-labelled tool message, and 256 KiB aggregate tool
  text, with a 30-second cooperative deadline by default (chat uses 120 seconds);
- records `tool.requested`, `tool.completed`, and schema-version-2 tool messages,
  and records the exact tool schema version and names in the request manifest;
- treats every tool result as source-labelled untrusted user-level content and
  never promotes it to the system role;
- denies every call with `model_tools_disabled` unless the workspace's live
  `model_tools` preference is still enabled;
- sends saved tool results from earlier turns again only while the live
  `model_tools` and `read` preferences both allow it, and otherwise replaces
  each with a fixed withheld notice;
- reports the final reply's provider `finish_reason`.

The runtime supports no writes, process execution, Git, MCP, or network access.
`LocalToolModel` adapts the LM Studio client to its model interface. The adapter
sends the tool definitions in the OpenAI-compatible `tools` field, parses the
response with the strict duplicate-key-rejecting parser, and returns each call
unvalidated for the runtime to check. Other provider adapters must do the same
and keep the subscription/API profile split. See the
[session contract](session-storage.md) for the event sequence and the
[file-access contract](file-access.md) for broker checks.

## Local chat

Start LM Studio separately and load the model you intend to use. Check the
loaded model's context capacity in LM Studio. A model's advertised maximum can
differ from its loaded capacity. The following example uses the user's loaded
google/gemma-4-e4b profile with a verified capacity of 131,072 tokens:

```text
cargo run --locked -p omgcode-cli -- chat --workspace . --model google/gemma-4-e4b --context-tokens 131072 --max-output-tokens 512 "Reply with READY."
```

The default endpoint is http://127.0.0.1:1234/v1. Override it with --endpoint
only for an explicitly selected local service. This prototype accepts HTTP,
literal loopback IP addresses, an explicit nonzero port, and the /v1 path.
Hostnames, remote addresses, redirects, and proxy discovery are disabled. A
loopback connection does not establish how the selected server handles data
upstream. Verify the server's own configuration before sending sensitive text.

The CLI sends the compiled base prompt, optional --instructions user-level
preferences, the saved user/assistant history, and the current message. When the
session's workspace does not enable `model_tools`, it sends no tool
definitions and rejects structured tool calls. Saved tool results from earlier
turns are not sent: each is replaced by a fixed withheld notice, and the journal
keeps the original. The output reports `model_tools.mode` as `disabled` with a
reason and a `withheld_tool_results` count. Text which resembles a command
remains text.

When the workspace enables `model_tools`, chat offers `workspace_read_file` and
`workspace_list_directory`. When `execute`, `read`, and `write` also allow it,
it offers the approval-gated `workspace_run_command`. The model may make up to
four requests in one turn. Each read/list call is rechecked against live
settings and stays inside the workspace. Results return to the model as
source-labelled user messages. The configured LM Studio token is removed from
each result before it is recorded or sent. A tool call that contains the token
is refused with a `provider` error before it is recorded, because editing its
arguments could change the requested path. Saved tool results are sent again
only while the live `model_tools` and `read` preferences both allow it. Chat
prompts for no approval: a command call first records a pending approval and
returns `approval_required`; the operator runs `approvals approve <id>` between
turns, and a second identical call consumes it and calls the trusted runner.
The output reports `model_tools.mode` as `enabled`, with the workspace ID,
rounds, call count, tool bytes, and withheld count. `reply.finish_reason` is `length` when the token
limit cut the final reply short; the turn is still recorded as complete. A refused or invalid call ends the turn with a `permission`,
`tool_call`, or `tool_limit` error, recorded in the journal.

```text
omgcode --data-dir C:\omgcode-data chat --journal <journal-path> --model google/gemma-4-e4b --context-tokens 131072 "Summarise README.md."
```

Output is JSON so untrusted control characters are escaped rather than written
as terminal instructions.

Each text-only invocation sends one non-streaming request; a tool-enabled turn
sends at most four. There is a five-second connect timeout and a thirty-second
timeout per request, with no automatic retry or provider fallback. A
tool-enabled turn also stops at a 120-second cooperative deadline. A slow first inference can time out. Its remote outcome can
be unknown, and a retry is a new explicit user action.

Optional server authentication can be supplied to the trusted CLI process with
OMGCODE_LM_TOKEN. The CLI retains the token in memory only and sends it as a
sensitive Authorization header. It does not save it or pass it to a child task.
Environment-based input is a temporary prototype interface, not protected
credential storage or a claim of memory erasure. OS-backed credentials and the
account/settings interface remain required. Use the no-token profile only for
an explicitly selected service configured for it.

## Saved sessions

By default, journals use the platform's per-user local application data
directory. A trusted operator can override this with --data-dir. Keep that
directory outside project folders, under an account-protected location. Do not
paste credentials into messages or preferences: these inputs are saved as text.

```text
omgcode session new --workspace . --title "Prototype test"
omgcode session inspect <journal-path>
omgcode chat --journal <journal-path> --model google/gemma-4-e4b --context-tokens 131072 "Continue the conversation."
omgcode session archive <journal-path>
omgcode session restore <journal-path>
```

Use the full journal path returned in the JSON output. The commands above use an
installed executable; during development, prefix each with cargo run --locked
-p omgcode-cli -- instead of omgcode. --instructions applies to the
current invocation and is recorded in its manifest. Resume does not silently
reuse earlier custom instructions or model settings. Supply them again when
needed. The current compiled base always replaces historical base snapshots.

Results also include a lossless journal_native object. The journal string is
null when the path cannot be represented as Unicode; journal_display is always
display text. Supply the serialized native object with --journal-native instead
of a positional path for session commands, or instead of --journal for chat.
Only the originating OS's encoding is accepted. Post-open chat failures include
the same locator under error.session, so a failed first request can be inspected
without searching for its journal or creating another session.

Archive and restore keep the same filename and session ID. They start no work.
Archiving an active turn fails. If the CLI stopped during a turn, inspect the
journal, then explicitly mark that turn failed before resuming or archiving:

```text
omgcode session interrupt <journal-path>
```

Interrupt acquires the exclusive journal lock. It cannot interrupt another
running writer. It records operator_interrupted for an abandoned turn, without
retrying a request or claiming remote rollback. Invalid or truncated journals
are rejected without modification. Automated repair and a recovery UI remain
planned. See the complete [session contract](session-storage.md).

## Implemented limits

| Resource | Prototype limit |
| --- | --- |
| Custom instructions | 16 KiB of UTF-8 text |
| Each user/assistant message | 16 KiB of UTF-8 text |
| History in one request | 128 messages |
| Assembled text / encoded request | 256 KiB each, checked separately |
| Encoded provider response | 1 MiB |
| Encoded journal event, including LF | 64 KiB |
| Journal | 8 MiB and 10,000 events |
| Tool call arguments in a session event | 4 KiB |
| Read file size per call | 15 KiB maximum, 8 KiB default |
| Edit preview content | 1 MiB per side |
| Edit preview lines | 65,536 per side when changed |
| Edit preview diff | 64 KiB |
| Source-labelled tool message | 16 KiB |
| Tool rounds per turn | 4 |
| Tool calls per round / per turn | 8 / 16 |
| Aggregate tool text per turn | 256 KiB |
| Cooperative turn deadline | 30 seconds by default; 120 seconds for chat |
| Raw tool calls accepted from one response | 64 before runtime validation |

These are ordinary quotas. Active turns reserve one terminal event and 1 KiB
for it. CLI requests preflight their encoded manifest and user event, and reserve
space for a maximum-size assistant event before recording the turn marker. A
deterministic reply-size failure closes the turn; an uncertain write/sync failure
stops the writer instead. For older journals already full with an active turn,
one terminal recovery event of at most 1 KiB may exceed the ordinary quotas.
Readers accept only that bounded exception. Full journals remain inspectable;
further ordinary events require a new session. No history is removed to make room.

The context guard estimates input from UTF-8 bytes plus fixed message overhead,
then reserves the configured output tokens and headroom. This is a conservative
heuristic, not a tokenizer or a guarantee for every chat template. The operator
must supply a validated loaded-model capacity. Compaction and automatic history
selection are not implemented: requests over a limit fail before inference.
Provider-reported usage and finish reason appear in the reply JSON. Durable
usage events, streaming chunks, and large-output artifacts remain planned.

## Code boundaries

| Crate | Responsibility |
| --- | --- |
| omgcode-core | Compiled prompt, bounded model adapter, session/control/file-operation/run journals, workspace policy and toolchain profiles, broker orchestration, trusted runs, bounded turn/tool lifecycle, and non-destructive edit previews |
| omgcode-cli | Trusted operator input, one-turn chat orchestration with optional read/list and approval-gated command model tools, bounded file and preview commands, trusted `run`, JSON output |
| omgcode-platform | Native journal validation, directory identity, handle-relative workspace file operations, Windows AppContainer grants, interrupt-to-cancel handling, and a process-tree runner with Linux Landlock/seccomp and Windows AppContainer confinement |

Core and CLI forbid unsafe code. The platform crate denies unsafe code except
inside its reviewed Windows handle wrappers for directory identity, native file
operations, process creation and job control, AppContainer profiles and ACL
grants, and console control handler registration, and its Linux pre-exec
`prctl`, seccomp, and `close_range` calls and `sigaction` registration. Linux uses Rustix's safe `openat2` API. Directory registration
uses native file IDs and requires directory creation-time metadata. The broker
reopens the registered root and workspace, checks their identities and creation
times, then accesses relative names through retained handles. These live checks
apply to broker calls; directory identity recording alone remains configuration
metadata.
Rust's stable metadata API does not
provide the needed Windows hard-link count. The wrapper borrows the existing
handle and checks GetFileInformationByHandle; it never takes ownership of a raw
handle. Linux broker traversal requires kernel 5.6 or later for `openat2` and
available `/proc/self/fd` support. Required beneath-root, no-symlink,
no-magic-link, and no-mount-crossing resolution has no weaker fallback.
Unsupported APIs fail closed. Windows uses native handle-relative and
reparse-point controls. Existing
parent directories must remain trusted. Windows ACL hardening, local filesystem
qualification, and power-loss durability are still required. The broker is not
whole-user or process containment.

Session workspace IDs in session journals are descriptive metadata. They grant
no access. Each scoped file operation supplies the workspace ID and session
UUID to the loaded control store; the broker checks the current binding, policy
revision, registration, and native directory identity. Audit and acknowledgement
commands use existing settings to inspect or resolve file-operation recovery;
they do not use a workspace or session binding. The control journal persists
workspace registrations, settings, revisions, and policy audit events. File
operations use a separate journal, so they do not change the `control-v1.jsonl`
or session JSONL schemas. A broader project registry and the folder/session tree remain
future work. The CLI's explicit journal path is a trusted operator interface;
it must not become a raw-path command in the future web interface.

## Dependencies

Direct dependencies have specific purposes:

| Dependency | Purpose and selected surface |
| --- | --- |
| serde / serde_json | Typed JSON serialization and bounded protocol parsing; derive macros run during compilation |
| uuid | Random UUIDv4 identifiers and typed JSON identifiers; OS-backed randomness through its dependency graph |
| libc (Unix only) | Locked at 0.2.191; supplies `O_DIRECTORY` and `O_NOFOLLOW` constants to safe `std::fs::OpenOptions` flag configuration, avoiding ABI-specific numeric flags without adding unsafe core code |
| rustix (Linux only) | Locked at 1.1.6 with the `fs` feature; supplies safe Rust wrappers for `openat2`, directory-relative opens, metadata, enumeration, and sync without adding unsafe Linux code |
| time | UTC date layout and RFC 3339 formatting/parsing |
| sha2 | Standard SHA-256 prompt digest; no custom cryptography |
| thiserror | Typed component errors; derive macro runs during compilation |
| reqwest | Blocking HTTP client; default features disabled, no TLS backend or hosted execution used in this loopback-only increment |
| clap | CLI parsing and help; derive macro runs during compilation |
| directories | Platform application-data location selection |
| windows-sys | Maintained Windows API bindings behind narrow reviewed wrappers for Win32/WDK Foundation and Storage/FileSystem, Security, I/O handles, processes and jobs, and console control events |
| tempfile | Private disposable test data; development dependency only |

The selected direct crates declare MIT and/or Apache-2.0 licensing and support
the build targets. Cargo.lock and each update's metadata need review; this is
not a full audit of transitive dependencies, unsafe code, build scripts, or
procedural macros. No Git dependencies or downloaded execution backends are used.
Use maintained libraries for HTTP, cryptography, and OS bindings instead of
custom implementations. Build dependencies execute with the developer's build
permissions, not with future OMGCode task restrictions.

## Verification

Run the [Cargo verification baseline](testing-standards.md#cargo-verification-baseline)
on both Windows and Linux. The Rust workflow runs formatting, strict Clippy,
unit/integration tests, and separate documentation tests. On Linux the run and
tree-limit tests start themselves in `systemd-run --user` scopes, so they need
a running user service manager; without one they fail. Tests use private data
and loopback fixtures. They do not call a live model. Live compatibility checks
are explicit, bounded checks of a particular server/model/auth profile.

The toolchain acceptance tests are ignored by default. After a change to runs,
profiles, or confinement, run them on both platforms with the toolchain
variables listed in `crates/omgcode-cli/tests/toolchain_acceptance.rs`:

```text
cargo test -p omgcode-cli --test toolchain_acceptance --locked -- --ignored
```

Do not infer whole-user/process containment, Git safety, credential storage,
release filesystem support, or full coding-agent support from these tests. The
broker supports bounded file operations through trusted CLI input and the core
turn runtime, which chat drives for read/list and an approval-gated command when
a workspace enables `model_tools`. Live provider tool-call compatibility and
process containment
remain separate boundaries.
