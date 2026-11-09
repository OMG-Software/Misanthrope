# Build and run the core prototype

OrangeCode 0.4.0 is a developer CLI prototype with workspace permission
settings and bounded native file operations through trusted operator commands.
It can send a text conversation to an explicitly selected local LM Studio model
and save that conversation. It can read/list files and exclusively create new
files/directories in a registered workspace. The core also has a bounded
turn/tool runtime for library callers and tests, but no CLI command exposes it.
The CLI cannot replace or delete existing files, run commands, use Git, or
connect to MCP servers. CLI/provider model tool dispatch and an execution
sandbox are not implemented.

## Build

Install Rust through rustup and use a native Windows or Linux development
environment. Windows needs the MSVC linker and Windows SDK, normally supplied by
Visual Studio Build Tools with Desktop development with C++. Linux needs a
working C linker. Run these commands from the repository root:

```text
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --workspace --locked
cargo run --locked -p orangecode-cli -- capabilities
cargo run --locked -p orangecode-cli -- prompt
```

The repository pins Rust 1.99.1, edition 2024, for all three crates. This is also
the declared minimum version; no older compiler is supported. Cargo.lock fixes
the resolved dependency graph. The executable is named orangecode.

## Application namespace and existing prototype data

The command is `orangecode`, the model-token variable is `ORANGECODE_LM_TOKEN`,
and new session filenames start with `orangecode-`. Default private storage uses
the OrangeCode application identity; Linux normalizes its directory name to
`orangecode`. The application does not search for or move previous data.

To keep existing workspace settings, select their original private directory
explicitly with `--data-dir`. Do not relocate the control journal: its recorded
directory path and native identity must still match. Configure a new store
through the trusted workspace commands if the old directory must move.

To reuse a session with a different filename prefix, first close all writers
and keep a private backup. Change only the filename prefix to `orangecode-`;
retain the timestamp, UUID, `.jsonl` extension, and all journal bytes. Then use
`session inspect` with its explicit path and the intended `--data-dir` before
continuing. The JSONL schemas are unchanged. Inspection does not grant workspace
access or replay actions. See [ADR-0014](adr/0014-use-the-renamed-application-namespace-without-automatic-migration.md).

## Workspace settings

Use a private application data directory outside your development root. These
examples use Windows paths; on Linux use separate paths such as
`/home/me/.local/share/orangecode` and `/home/me/dev`. Create the root and
project directories first. Each command returns JSON. Use the current revision
from its output for the next mutation; replace WORKSPACE_ID and SESSION_UUID
with returned workspace ID and a UUIDv4 session selection.

```text
orangecode --data-dir C:\orangecode-data workspace status
orangecode --data-dir C:\orangecode-data workspace set-root C:\dev --revision 0
orangecode --data-dir C:\orangecode-data workspace register C:\dev\project --revision 1
orangecode --data-dir C:\orangecode-data workspace permissions WORKSPACE_ID --revision 2 --write false
orangecode --data-dir C:\orangecode-data workspace check WORKSPACE_ID --session SESSION_UUID --operation read
orangecode --data-dir C:\orangecode-data workspace audit --limit 20
```

`status` initializes an absent store at revision zero. `check` and `audit`
require existing state. Flags take explicit `true` or `false` values. Read/write
default to enabled; execute/task-network default to disabled. All decisions
still report `execution_allowed: false`. Commands do not dispatch tools.
See the [workspace contract](workspace-permissions.md) for limits and failures.

When a root is configured, new sessions use stable IDs from the persisted
workspace registration. Invalid settings or outside-root selections fail.
Existing text-only journals remain readable without inferred access grants.
With no configured root, the earlier text-only behavior remains available and
sets no implicit root. Registrations store settings and audit history; they do
not create project folders or grant access. A broader project registry and the
folder/session tree remain future work.

## Workspace files

Scoped file commands (`read`, `list`, `create`, and `mkdir`) require existing
workspace settings, a configured development root, a registered workspace ID,
and an explicit session UUID. They do not initialize settings, infer a root, or
treat a journal's workspace metadata as a grant. `read` and `list` require the
read preference. `create` and `mkdir` require the write preference. Execute
remains disabled. The `audit` and `acknowledge` recovery commands require only
existing workspace settings. They do not require a configured root, workspace
registration, or session selection. Chat advertises no file tools.

Capability JSON separates implementation from availability. On Linux and
Windows, `workspace_file_broker_implemented` is `true`, while
`workspace_file_broker` and `exclusive_file_creation` are `null` (unknown). On
other platforms, all three values are `false`. The CLI does not probe the ambient
filesystem for capability reporting. A command checks settings, scope, policy,
and native requirements when it runs. These values do not promise that an
operation can proceed. `existing_file_replacement` and `workspace_file_tools`
remain `false`.

```text
orangecode --data-dir C:\orangecode-data file read WORKSPACE_ID --session SESSION_UUID src\main.rs --max-bytes 65536
orangecode --data-dir C:\orangecode-data file list WORKSPACE_ID --session SESSION_UUID src --limit 100
orangecode --data-dir C:\orangecode-data file create WORKSPACE_ID --session SESSION_UUID notes.txt --content "Review this file"
orangecode --data-dir C:\orangecode-data file mkdir WORKSPACE_ID --session SESSION_UUID notes
orangecode --data-dir C:\orangecode-data file audit --limit 20
orangecode --data-dir C:\orangecode-data file acknowledge OPERATION_UUID
orangecode --data-dir C:\orangecode-data file rotate --archive-dir C:\private-audit-exports
orangecode --data-dir C:\orangecode-data file audit --archive ARCHIVE_PATH --limit 20
```

Omit the list path to list the selected workspace root. The empty path is not
valid for read, create, or mkdir. Reads return bounded bytes as untrusted data.
Creation refuses every existing entry. File-operation intents and outcomes use
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

`orangecode_core::turn` provides a session-owned runtime for one bounded
model/tool turn. It is not a CLI command and `chat` still advertises no tools.
A caller supplies a model implementation, an open schema-version-2 session, the
trusted application-data directory, and the workspace ID. The runtime:

- parses strict OpenAI-style function calls for `workspace_read_file` and
  `workspace_list_directory`;
- rejects unknown tools, malformed or duplicate arguments, absolute or parent
  paths, and duplicate call identifiers before dispatch;
- opens the control store for each broker call, obtains a fresh session binding,
  and releases the settings lock before the next model request;
- enforces hard maxima of four rounds, eight calls per round, sixteen calls per
  turn, 16 KiB per source-labelled tool message, and 256 KiB aggregate tool
  text, with a 30-second cooperative deadline by default;
- records `tool.requested`, `tool.completed`, and schema-version-2 tool messages,
  and records the exact tool schema version and names in the request manifest;
- treats every tool result as source-labelled untrusted user-level content and
  never promotes it to the system role.

The runtime supports no writes, process execution, Git, MCP, or network access.
Its model interface is a test seam, not a provider adapter. A future adapter
must parse provider JSON strictly, map its tool-call response without trusting
provider metadata, and keep the subscription/API profile split. See the
[session contract](session-storage.md) for the event sequence and the
[file-access contract](file-access.md) for broker checks.

## Local chat

Start LM Studio separately and load the model you intend to use. Check the
loaded model's context capacity in LM Studio. A model's advertised maximum can
differ from its loaded capacity. The following example uses the user's loaded
google/gemma-4-e4b profile with a verified capacity of 131,072 tokens:

```text
cargo run --locked -p orangecode-cli -- chat --workspace . --model google/gemma-4-e4b --context-tokens 131072 --max-output-tokens 512 "Reply with READY."
```

The default endpoint is http://127.0.0.1:1234/v1. Override it with --endpoint
only for an explicitly selected local service. This prototype accepts HTTP,
literal loopback IP addresses, an explicit nonzero port, and the /v1 path.
Hostnames, remote addresses, redirects, and proxy discovery are disabled. A
loopback connection does not establish how the selected server handles data
upstream. Verify the server's own configuration before sending sensitive text.

The CLI sends the compiled base prompt, optional --instructions user-level
preferences, the saved user/assistant history, and the current message. It sends
no workspace contents or tool definitions. It rejects structured tool calls.
Text which resembles a command remains text. Output is JSON so untrusted control
characters are escaped rather than written as terminal instructions.

Each invocation sends one non-streaming request. There is a five-second connect
timeout and a thirty-second total request timeout, with no automatic retry or
provider fallback. A slow first inference can time out. Its remote outcome can
be unknown, and a retry is a new explicit user action.

Optional server authentication can be supplied to the trusted CLI process with
ORANGECODE_LM_TOKEN. The CLI retains the token in memory only and sends it as a
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
orangecode session new --workspace . --title "Prototype test"
orangecode session inspect <journal-path>
orangecode chat --journal <journal-path> --model google/gemma-4-e4b --context-tokens 131072 "Continue the conversation."
orangecode session archive <journal-path>
orangecode session restore <journal-path>
```

Use the full journal path returned in the JSON output. The commands above use an
installed executable; during development, prefix each with cargo run --locked
-p orangecode-cli -- instead of orangecode. --instructions applies to the
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
orangecode session interrupt <journal-path>
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
| Source-labelled tool message | 16 KiB |
| Tool rounds per turn | 4 |
| Tool calls per round / per turn | 8 / 16 |
| Aggregate tool text per turn | 256 KiB |
| Cooperative turn deadline | 30 seconds |

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
| orangecode-core | Compiled prompt, bounded model adapter, session/control/file-operation journals, workspace policy, broker orchestration, and bounded turn/tool lifecycle |
| orangecode-cli | Trusted operator input, one-turn text chat orchestration, bounded file commands, JSON output |
| orangecode-platform | Native journal validation, directory identity, and handle-relative workspace file operations |

Core and CLI forbid unsafe code. The platform crate denies unsafe code except
inside its reviewed Windows handle wrappers for directory identity and native
file operations. Linux uses Rustix's safe `openat2` API. Directory registration
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
| windows-sys | Maintained Windows API bindings behind narrow reviewed wrappers for Win32/WDK Foundation and Storage/FileSystem, Security, and I/O handles |
| tempfile | Private disposable test data; development dependency only |

The selected direct crates declare MIT and/or Apache-2.0 licensing and support
the build targets. Cargo.lock and each update's metadata need review; this is
not a full audit of transitive dependencies, unsafe code, build scripts, or
procedural macros. No Git dependencies or downloaded execution backends are used.
Use maintained libraries for HTTP, cryptography, and OS bindings instead of
custom implementations. Build dependencies execute with the developer's build
permissions, not with future OrangeCode task restrictions.

## Verification

Run the [Cargo verification baseline](testing-standards.md#cargo-verification-baseline)
on both Windows and Linux. The Rust workflow runs formatting, strict Clippy,
unit/integration tests, and separate documentation tests. Tests use private data
and loopback fixtures. They do not call a live model. Live compatibility checks
are explicit, bounded checks of a particular server/model/auth profile.

Do not infer whole-user/process containment, Git safety, credential storage,
release filesystem support, or full coding-agent support from these tests. The
broker supports bounded file operations through trusted CLI input and the core
turn runtime. CLI/provider model dispatch and process containment remain
separate implementation boundaries.
