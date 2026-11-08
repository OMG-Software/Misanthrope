# Build and run the core prototype

Misanthrope 0.2.0 is a developer CLI prototype with workspace permission settings.
It can send a text conversation
to an explicitly selected local LM Studio model and save that conversation.
It is not yet a coding agent: it cannot read project contents, edit files, run
commands, use Git, or connect to MCP servers. No execution sandbox exists yet.

## Build

Install Rust through rustup and use a native Windows or Linux development
environment. Windows needs the MSVC linker and Windows SDK, normally supplied by
Visual Studio Build Tools with Desktop development with C++. Linux needs a
working C linker. Run these commands from the repository root:

```text
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --workspace --locked
cargo run --locked -p misanthrope-cli -- capabilities
cargo run --locked -p misanthrope-cli -- prompt
```

The repository pins Rust 1.99.1, edition 2024, for all three crates. This is also
the declared minimum version; no older compiler is supported. Cargo.lock fixes
the resolved dependency graph. The executable is named misanthrope.

## Workspace settings

Use a private application data directory outside your development root. These
examples use Windows paths; on Linux use separate paths such as
`/home/me/.local/share/misanthrope` and `/home/me/dev`. Create the root and
project directories first. Each command returns JSON. Use the current revision
from its output for the next mutation; replace WORKSPACE_ID and SESSION_UUID
with returned workspace ID and a UUIDv4 session selection.

```text
misanthrope --data-dir C:\misanthrope-data workspace status
misanthrope --data-dir C:\misanthrope-data workspace set-root C:\dev --revision 0
misanthrope --data-dir C:\misanthrope-data workspace register C:\dev\project --revision 1
misanthrope --data-dir C:\misanthrope-data workspace permissions WORKSPACE_ID --revision 2 --write false
misanthrope --data-dir C:\misanthrope-data workspace check WORKSPACE_ID --session SESSION_UUID --operation read
misanthrope --data-dir C:\misanthrope-data workspace audit --limit 20
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

## Local chat

Start LM Studio separately and load the model you intend to use. Check the
loaded model's context capacity in LM Studio. A model's advertised maximum can
differ from its loaded capacity. The following example uses the user's loaded
google/gemma-4-e4b profile with a verified capacity of 131,072 tokens:

```text
cargo run --locked -p misanthrope-cli -- chat --workspace . --model google/gemma-4-e4b --context-tokens 131072 --max-output-tokens 512 "Reply with READY."
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
MISANTHROPE_LM_TOKEN. The CLI retains the token in memory only and sends it as a
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
misanthrope session new --workspace . --title "Prototype test"
misanthrope session inspect <journal-path>
misanthrope chat --journal <journal-path> --model google/gemma-4-e4b --context-tokens 131072 "Continue the conversation."
misanthrope session archive <journal-path>
misanthrope session restore <journal-path>
```

Use the full journal path returned in the JSON output. The commands above use an
installed executable; during development, prefix each with cargo run --locked
-p misanthrope-cli -- instead of misanthrope. --instructions applies to the
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
misanthrope session interrupt <journal-path>
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
| misanthrope-core | Compiled prompt, bounded model adapter, session/control journals, workspace settings and policy |
| misanthrope-cli | Trusted operator input, one-turn orchestration, JSON output |
| misanthrope-platform | Native journal-handle validation and directory identity |

Core and CLI forbid unsafe code. The platform crate denies unsafe code except
inside its reviewed Windows handle wrappers. Directory registration uses
128-bit native file IDs and requires directory creation-time metadata.
Rust's stable metadata API does not
provide the needed Windows hard-link count. The wrapper borrows the existing
handle and checks GetFileInformationByHandle; it never takes ownership of a raw
handle. Unix uses standard file metadata. These checks reject known unsafe
journal objects; they do not establish a development-root access broker or an
execution boundary. Existing parent directories must remain trusted. Windows
ACL hardening and power-loss durability qualification are still required.

Session workspace IDs are descriptive metadata. They grant no access. The
control journal persists workspace registrations, settings, revisions, and
audit events. A broader project registry and the folder/session tree remain
future work. The CLI's explicit journal path is a trusted operator interface;
it must not become a raw-path command in the future web interface.

## Dependencies

Direct dependencies have specific purposes:

| Dependency | Purpose and selected surface |
| --- | --- |
| serde / serde_json | Typed JSON serialization and bounded protocol parsing; derive macros run during compilation |
| uuid | Random UUIDv4 identifiers and typed JSON identifiers; OS-backed randomness through its dependency graph |
| libc (Unix only) | Locked at 0.2.191; supplies `O_DIRECTORY` and `O_NOFOLLOW` constants to safe `std::fs::OpenOptions` flag configuration, avoiding ABI-specific numeric flags without adding unsafe core code |
| time | UTC date layout and RFC 3339 formatting/parsing |
| sha2 | Standard SHA-256 prompt digest; no custom cryptography |
| thiserror | Typed component errors; derive macro runs during compilation |
| reqwest | Blocking HTTP client; default features disabled, no TLS backend or hosted execution used in this loopback-only increment |
| clap | CLI parsing and help; derive macro runs during compilation |
| directories | Platform application-data location selection |
| windows-sys | Maintained Windows API bindings, Windows-only Foundation and Storage/FileSystem features |
| tempfile | Private disposable test data; development dependency only |

The selected direct crates declare MIT and/or Apache-2.0 licensing and support
the build targets. Cargo.lock and each update's metadata need review; this is
not a full audit of transitive dependencies, unsafe code, build scripts, or
procedural macros. No Git dependencies or downloaded execution backends are used.
Use maintained libraries for HTTP, cryptography, and OS bindings instead of
custom implementations. Build dependencies execute with the developer's build
permissions, not with future Misanthrope task restrictions.

## Verification

Run the [Cargo verification baseline](testing-standards.md#cargo-verification-baseline)
on both Windows and Linux. The Rust workflow runs formatting, strict Clippy,
unit/integration tests, and separate documentation tests. Tests use private data
and loopback fixtures. They do not call a live model. Live compatibility checks
are explicit, bounded checks of a particular server/model/auth profile.

Do not infer process containment, permission revocation, Git safety, credential
storage, or full coding-agent support from these tests. Those controls and their
native-platform acceptance tests are the next implementation boundaries.
