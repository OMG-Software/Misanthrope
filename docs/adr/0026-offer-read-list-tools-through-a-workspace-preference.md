# 0026: Offer read/list tools through a workspace preference

- Status: Accepted
- Date: 2026-11-11

## Context

ADR-0016 added a bounded turn runtime that dispatches only the read and list
broker calls. It had no CLI command and no real provider adapter. The LM Studio
adapter refused every tool call. Phase B of the implementation plan required an
explicit product decision before chat could offer model tools.

The maintainer chose these rules:

- A persisted per-workspace preference turns the tools on. There is no
  per-run flag.
- A tool call needs no per-call confirmation.
- A model tool call must never read outside the user-selected top-level folder.

## Decision

Add a `model_tools` preference to each workspace's permission set. It is off by
default. The trusted command `workspace permissions <id> --model-tools true`
changes it like any other preference: it needs the expected revision, appends
a `permissions_changed` event, and increments the revision. Prompts, journals,
and model output cannot change it.

`model_tools` is not an operation class. `workspace check` and policy audit
records are unchanged. The serialized field is omitted while it is `false`, so
settings that never enable it keep their exact bytes. Older readers can still
open those settings. Once the preference is enabled, version 0.11.0 refuses the
settings log as an unknown field. `workspace status` always reports the flag.

`chat` decides the tool mode from live settings before each turn. The selected
session's recorded workspace UUID can only select an existing registration. The
workspace ID and preference come from current settings. Tools are offered only
when all of these hold:

- the build runs on Linux or Windows, matching the `model_read_list_tools`
  capability;
- a control store exists;
- the recorded UUID matches a registered workspace;
- that workspace's `model_tools` preference is enabled;
- the session journal uses schema version 2.

Otherwise chat stays text only and reports the reason. Invalid settings fail
before any model request.

When tools are offered, chat runs the ADR-0016 runtime with a new LM Studio
adapter method. The adapter sends the two tool definitions in the
OpenAI-compatible `tools` field with `tool_choice` set to `auto`. It parses the
response with the strict duplicate-key-rejecting parser. It refuses legacy
`function_call` output, choice-level calls, tool calls in a reply cut off by the
token limit, and more than 64 calls. It returns each call unvalidated. Tool
definitions count against the context estimate and the 256 KiB request limit.

Before every call, the runtime opens the control store again and creates a
fresh binding. It denies the call with `model_tools_disabled` unless the live
preference is still enabled. The broker then requires the live `read`
preference. A change made during model I/O takes effect at the next call.
Calls stay confined to the session workspace in two layers:

- Parsing rejects absolute, drive-qualified, root, current-directory, and
  parent paths before any dispatch.
- The native broker refuses links, junctions, hard links, and names outside the
  registered workspace, which must lie beneath the development root.

Tool results stay source-labelled user content. The compiled base prompt
remains the only system message. Chat removes a configured local server token
from text sent to and received from the model, as text chat already does. A
tool call whose identifier or arguments contain the token is refused before the
runtime records it; redacting the arguments could change the requested path.

Earlier tool results are workspace data disclosed under an earlier decision. A
turn sends them again only while the live `model_tools` and `read` preferences
both allow it, checked when the turn starts. Otherwise each is replaced by a
fixed withheld notice, and the journal keeps the original. Text-only chat
always withholds them.

A final reply cut short by the token limit is accepted, as in text chat. Its
`finish_reason` of `length` is reported in the output, not hidden.

## Alternatives considered

- A per-run `--workspace-tools` flag: rejected by the maintainer. A persisted
  preference is the only switch.
- Both a preference and a per-run flag: rejected as two switches for one
  decision.
- Confirm each call on the terminal: rejected by the maintainer. Read and list
  are bounded, policy-checked, journalled, and confined to the workspace.
- Make `model_tools` an operation class: rejected. Audit records would change
  shape, and a diagnostic decision is not a grant.
- Always serialize `model_tools`: rejected. Every later settings change would
  then make the log unreadable by 0.11.0, even when tools are never enabled.
- Build the workspace ID directly from the journal's UUID: rejected. A recorded
  value selects a registration and grants nothing.
- Fall back to text-only chat when settings are invalid: rejected. Invalid
  settings fail closed, as they do for session creation.
- Send tool results back with the provider `tool` role: rejected. ADR-0016 keeps
  tool content user-level and source-labelled.
- Replay saved tool results whatever the current preferences: rejected after
  review. It would keep disclosing file contents after the operator revoked
  access.
- Refuse to continue a session after revocation: rejected. Withholding the
  results keeps the conversation usable without sending them.
- Reject a reply cut short by the token limit in tool mode: rejected. Text chat
  accepts it and reports the reason; tool mode now does the same.

## Consequences

Version 0.12.0 lets a local LM Studio model read and list files in a workspace
whose operator enabled `model_tools`. No write, process, network, Git, or MCP
tool exists.

Limits and gaps:

- The journal selects the workspace. A crafted journal can select a different
  registered workspace that has `model_tools` enabled. Calls stay inside that
  workspace and need its live preferences. The chat output names the
  workspace ID used.
- Workspace text reaches the model as untrusted content. Prompt-injection
  defence beyond source labelling and the fixed tool set is not added here.
- Live compatibility with LM Studio tool-call output is not qualified. Mock
  fixtures cover the documented OpenAI-compatible shape only. A server that
  adds fields to a call object fails closed. One opt-in Windows spot-check with
  LM Studio 0.4.28 and `google/gemma-4-e4b` passed read, list, revocation, and path-refusal
  cases; it does not qualify other versions or models.
- A same-user process can still change files between calls. Reads are
  snapshots, not locks.
- Secret redaction covers only the configured local server token. Chat
  removes it from tool results before they are recorded or sent, and from model
  text, and refuses tool calls that contain it. Other secrets in a workspace
  file reach the journal and the model.
- Withholding earlier tool results cannot recall what the model already
  received. A permission change during a turn affects the next call, not
  history already sent in that turn.

## References

- [Workspace settings](../workspace-permissions.md#model-readlist-and-command-tools)
- [ADR-0012](0012-separate-workspace-settings-from-execution-authority.md)
- [ADR-0013](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md)
- [ADR-0016](0016-add-a-session-owned-tool-runtime-with-schema-v2.md)
