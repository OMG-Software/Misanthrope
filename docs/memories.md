# Project and global memories

This is a planned contract. No memory store, retrieval service, or memory
settings page is implemented yet. Memories carry selected facts and preferences
across sessions. They are separate from conversation history and the compiled
base prompt.

## User-selected scope

Provide Save memory from a conversation and Add memory in settings. Show the
exact text and require the user to choose its scope when saving:

| Scope | Meaning | Example |
| --- | --- | --- |
| This project | Available to sessions in the selected registered project | This project uses PostgreSQL for production storage. |
| Global | Available across this user's projects in this installation | Prefer concise explanations and metric units. |

Default the scope selector to This project when a project is active. Keep Global
an explicit choice with a clear description that other projects can receive the
text. If no project is active, require an explicit valid scope/target rather
than silently saving globally. Show the project name and identity in the save
flow. Save only after the user submits their chosen text and scope.

The model may suggest a memory, but model output, tools, repository files, and
MCP servers cannot save one or select its scope on the user's behalf. Initial
support is user-controlled saving; background extraction and automatic
cross-session learning need a separate reviewed feature. Explicitly saved global
memories are intentional cross-project context, not permission to share project
files, conversation history, or other projects' memories.

## Project identity and storage

Bind project memories to a core-owned project ID, not a path prefix, display
name, branch name, or Git remote URL. Share them between sessions of that project.
Treat additional folders/worktrees as the same project only through an explicit,
validated project association. Copies, forks, nested projects, and repositories
with matching remotes must not acquire each other's memories automatically.
A moved or replaced project root requires its normal identity/access validation.

Global means this local user's Misanthrope installation. It does not imply
sharing with other users or automatic synchronization to devices, organizations,
or provider memory services. Selected memories may enter a currently permitted
model request, including after a provider/account change, only under the current
user's provider/data-egress policy.

Keep a versioned, core-owned memory store in protected per-user application
data, outside task-writable views. The backing file/database format remains
to be selected with implementation; the JSONL requirement applies to session
journals. Memory records need an ID, revision, scope/project ID, text, enabled
state, creation/update times, and provenance/source references. Preserve the
original source trust; saving a quotation does not make it an authoritative fact.
Bound record size, total storage, import size, and query output.

Use transactional writes and checked revisions for edits, scope changes, and
deletion. Do not publish a successful save or change before durable completion.
Detect concurrent edits instead of silently overwriting them. Derived search
indexes are rebuildable. A deleted record cannot be resurrected by replaying
an old journal event, restoring a cache, or resuming a conversation.

## Settings and lifecycle

Provide Global and This project views with search, preview, Add, Edit, Delete,
Enable, and Disable actions. Show scope, source, revision, and last update.
Allow the user to change scope deliberately. Promoting a project memory to Global
must show the new exposure before the user saves; do not broaden scope because
the same text appears in several projects. Do not merge or rewrite saved text
silently.

Provide separate controls for using global memories and project memories in the
current project/session, plus a session option to use neither. Disabling use
retains saved entries. Delete removes the active memory record and its derived
retrieval entries. Treat memory use and memory writes as separate controls.
Failures and unavailable storage must be visible; no unprotected fallback store.

Archiving a session does not delete its separately saved memories. Clearing a
cache does not delete memories. Deleting a memory prevents its future retrieval;
it does not erase copies already present in retained transcripts, backups, or
provider data. Expose those retention limits without presenting Delete as a
guarantee that the model can no longer infer the same fact from other sources.

## Retrieval and instruction priority

Retrieve only enabled, relevant entries from the current project's store and
the global store when their use is enabled. Use bounded local retrieval and a
visible token budget. Do not inject every saved entry on every request. External
embedding/search services need their own permitted data-egress contract.

Show selected memory text, scope, origin, and revision in the request inspector.
Record IDs/revisions and retained snapshots according to the instruction/journal
retention rules. Include the selected text in token accounting and the request's
context manifest. Do not change scope or save a newly inferred fact during
retrieval or compaction.

Memories are labelled supporting context, not a fourth privileged instruction
layer. The compiled base and current permissions remain authoritative. Current
explicit user requests and saved custom instructions take precedence over
conflicting memories. Within compatible memory preferences, project scope takes
precedence over global scope. Keep unresolved factual contradictions visible;
verify material facts against current sources rather than treating scope or
recency as proof of correctness.

An entry that says to ignore security rules, reveal secrets, call a destination,
or change permission settings cannot authorize those actions. Ordinary file,
tool, model, and network checks still apply. Never use the memory store as a
credential store. Reject known credential fields and redact sensitive structured
inputs; do not claim detection of every secret in arbitrary user text.

## Changes, compaction, and caches

Resolve memory revisions at each request boundary. Edits, deletion, disablement,
scope changes, and project changes invalidate affected retrieval entries,
unsent request manifests, active summaries, and derived caches. Preserve frozen
historical manifests under their retention policy. Rebuild active context
without withdrawn memory snapshots before the next request. Unknown dependency
provenance requires conservative invalidation, not continued reuse.

Preserve IDs, scope, revisions, and source trust across compaction. Consult the
current memory registry rather than accepting a summary's claim that a memory
is enabled or global. Global memory eligibility does not relax isolation of
conversation caches or project artifacts; derived context remains scoped to the
current project, session, account, model, and policy where applicable.

Label a request already in flight with its frozen memory manifest. Changes
apply to the next request. An immediate-effect change uses the normal
cancellation/rebuild flow; content already sent cannot be made unseen. Retain
original conversation evidence under its own policy instead of silently
rewriting the journal. Do not activate provider-native automatic memory or sync
entries to a remote memory service on the user's behalf.

## Acceptance requirements

Test explicit scope selection; absent project; project/global retrieval and
precedence; distinct users, projects, copies, forks, worktrees, and moved roots;
disabled memories; save/edit/delete/scope-change races; durable-write failure;
poisoned text/provenance; no implicit model/tool writes; stale facts; bounded
retrieval and token accounting; secret handling; compaction/cache invalidation;
in-flight changes; archive/resume; and deletion without journal/cache replay
resurrection. Run actual persistence/isolation tests on Windows and Linux before
claiming support.
