# 0040: Gate the model command tool with approvals and the trusted runner

- Status: Accepted
- Date: 2026-11-14

## Context

Version 0.24.0 added trusted per-call approval records but no model tool
consumed one. The next model-authorised capability is a command tool: the
local model may request one confined program run under a recorded toolchain
profile. Chat is one non-interactive turn, so the operator approves between
turns rather than inside a prompt.

Every process must still go through `omgcode_core::run::run`, the trusted
runner that applies live preferences, the profile, the run journal, and native
confinement. Prompt text and model claims grant nothing.

## Decision

Add the model tool `workspace_run_command`:

1. **Offering:** the runtime advertises it only when the session workspace's
   live `model_tools`, `execute`, `read`, and `write` preferences are enabled.
   Each call rechecks them; the run checks them again.
2. **Arguments:** `profile`, `program`, `arguments` (at most 64 strings), and
   an optional workspace-relative `cwd`. Parsing is strict: unknown fields,
   invalid names, control characters in the program, oversized arguments, and
   parent or absolute `cwd` values are refused. There is no shell; arguments
   are passed exactly.
3. **Fixed limits:** the tool does not accept model-selected limits. It uses a
   20-second deadline, 4 KiB retained from each output stream, 1 GiB memory,
   64 processes, 20 seconds of CPU, and 256 MiB of disk growth. It selects no
   network or host services.
4. **Approval binding:** the request digest covers the session, workspace,
   profile, program, exact arguments, working directory, and fixed limits.
   The operation is `command`.
5. **First call:** if no current approval matches, the runtime records a
   pending approval with a bounded summary that contains no argument text and
   returns `approval_required` with its identifier. Nothing runs.
6. **Approved call:** a matching approved, unexpired, unconsumed record is
   consumed immediately before the run. The runtime drops the settings lock,
   then calls the trusted runner. A settings change invalidates the approval.
7. **Denied, expired, and consumed records:** a denial returns
   `approval_denied` without a new request. An expired or consumed record
   causes a new pending request. A failed run consumes the approval; the model
   or operator must request a new one.
8. **Records:** the session journal records the tool intent and result; the run
   journal records the run intent and outcome. Output is untrusted, bounded,
   and returned to the model. No replay executes a recorded call.

The tool does not add a write, network, Git, or MCP tool. The `run` journal and
native confinement remain the authority and the only process path.

## Alternatives considered

- **Prompt-only approval or a model claim of approval.** Rejected: it is not
  authority.
- **Automatic approval for a session or workspace.** Rejected: it authorizes
  more than one exact request.
- **Model-selected limits or network access.** Rejected: the model cannot
  widen its own resource or network boundary.
- **A shell command string.** Rejected: it would re-parse untrusted text and
  break exact argument handling.
- **Direct unconfined execution from the model tool.** Rejected: the trusted
  runner is the only process path.
- **A UI approval inside the chat turn.** Deferred: chat is non-interactive
  today; the trusted CLI approval is the first operator surface.

## Consequences

- The operator flow is two turns: the model requests, the operator approves or
  denies, then the model retries. The CLI exposes `approvals` commands; no UI
  exists.
- The model can request a command but cannot approve it. A denied exact request
  stays denied for the current settings revision.
- A run that fails after consumption uses the approval. Re-requesting is
  explicit.
- The approval summary identifies the profile, program name, and argument
  count, not the arguments. The operator must inspect the session tool intent
  for the exact arguments before approving; the tool intent is untrusted
  evidence.
- The tests cover parsing, the permission gate, approval creation, consumption,
  and the run hand-off. A CLI acceptance test now drives the real chat process
  through both turns with a mock provider and an operator approval, then runs a
  successful confined command through the trusted runner on both Linux and
  Windows. It needs a delegated cgroup on Linux and a workspace container grant
  on Windows; a host without them fails the case rather than skipping it.
- A model command can return up to 4 KiB per stream; the turn's result and
  aggregate limits still apply, and a larger result is replaced with a
  controlled `output_too_large` result.

## References

- [Command execution](../execution.md#model-command-tool)
- [Agent instructions](../agent-instructions.md#approval-records)
- [Workspace settings](../workspace-permissions.md#model-readlist-and-command-tools)
- [Security coding rules](../security-coding-rules.md#process-and-sandbox-boundary)
- [Testing standards](../testing-standards.md)
- [ADR-0039](0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
