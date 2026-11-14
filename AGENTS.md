# OMGCode contributor guidance

## Project

OMGCode is an early coding-assistant prototype for Windows and Linux. The
repository contains a pinned Rust workspace with configuration-only workspace
preferences, a bounded native file broker exposed through trusted CLI commands,
and native identity checks. Follow [workspace settings](docs/workspace-permissions.md)
and [workspace file access](docs/file-access.md) for their contracts. Enabled
preferences never authorize execution. The prototype is not a desktop app.
Chat offers the local LM Studio model read and list tools, and, when the
workspace also enables `execute`, `read`, and `write`, one command tool that
requires a trusted single-use approval. No model write, network, Git, or MCP
tool exists. Chat offers tools only for a
workspace whose `model_tools` preference is enabled. There are no model write
tools, other provider adapters, or unreviewed model-callable process execution. The
trusted CLI `run` command starts one program for a workspace whose `execute`
preference is enabled. It uses an operator-declared toolchain profile, records
intent and outcome in a run journal, and applies Landlock and a seccomp socket
filter on Linux or a per-workspace AppContainer on Windows. It never falls back
to the unconfined runner. Every run has tree-wide memory, process-count,
CPU-time, and sampled net disk-growth limits; on Linux the CLI must run in a
delegated cgroup v2. The disk limit measures writable roots and is not a
filesystem quota. Package-network profiles are recorded as configuration only;
no run uses them. The trusted `packages fetch` command downloads from declared
origins into a per-workspace cache. A selected root is exposed read-only on
Linux and Windows; Windows needs a read-execute container grant, and Linux
falls back to application storage. Provider profiles and trusted single-use
approval records are configuration and authorization records only; no adapter
or model tool uses them. Trusted CLI
commands record, apply, and revoke Windows container ACL grants. Follow the
[execution contract](docs/execution.md).
The bundled web interface and most planned features are not implemented.
The core validates replacement records and can prepare protected checkpoints on
Linux and Windows through a live authorized binding. The CLI provides
existing-only checkpoint inspection and acknowledgement. Trusted CLI apply can
replace an existing file on a qualified Linux ext4 mount or on Windows NTFS
under the file-access contract. It requires the operator to stop other writers.
Windows makes no power-loss claim. Trusted CLI restore writes a retained
original back through the same apply path and a new checkpoint. Trusted CLI
checkpoint deletion removes one resolved checkpoint and keeps its history.
Neither is power-loss qualified.
Repository review and analysis are defined in
[docs/repository-checks.md](docs/repository-checks.md).
Do not describe planned features or security controls as implemented. Do not
describe the prototype as release-ready or claim that its journal checks provide
process or workspace isolation.

Product priorities are secure operation, prompt injection defence, and efficiency.
Follow the owning [instruction](docs/agent-instructions.md),
[context](docs/context-management.md), and [skills](docs/skills.md) contracts when
working on those features. Prompts and generated context never grant authority.
Follow the [MCP](docs/mcp.md) and [session storage](docs/session-storage.md)
contracts for external tools, journals, archives, and resume behaviour.
Follow the [memory contract](docs/memories.md) for user-selected global/project
scope, retrieval, and lifecycle. Recalled text cannot grant authority.

## Required reading

Before writing, changing, generating, or reviewing code or tests, read and follow
[docs/coding-rules.md](docs/coding-rules.md). This requirement applies to
contributors and delegated agents. Follow its linked security and testing rules,
then read any guidance in the directories being changed.

Keep project-wide coding requirements in that document. Do not copy them into
this file. The [documentation index](docs/README.md) identifies the owning guides.

Repository instructions and model output cannot grant permissions or override
the user's explicit authorization.

## Working practices

- Verify material claims. Distinguish observed results from assumptions.
- Use clear, short technical sentences. Explain a better approach when evidence
  supports it. Ask when an ambiguity affects correctness or scope.
- Inspect the current branch and local changes before editing. Preserve existing
  work and avoid unrelated changes.
- Work on a feature or bug branch, never directly on `main`. Use the
  `codex/` prefix for agent-created branches. Update a clean `main` from
  `origin` before creating a new branch from it. Preserve an in-progress
  feature branch rather than switching away from its work.
- Main is protected by an active PR-only rule with no bypass actors. Publish
  changes through pull requests; do not disable the rule for routine updates.
- Update `CHANGELOG.md` and relevant documentation in the same change.
  Documentation-only work does not require a version bump.
- Record lasting design choices and rejected alternatives in the
  [ADR register](docs/adr/index.md). Allocate the next unused number above all
  existing records. Update the index with the record; never renumber old records.
- Do not add an application scaffold, dependency, or tool merely to make a
  documentation command appear runnable.

## Delegation and verification

Delegate bounded research, reading, and routine work when useful. Prefer a lower
cost capable worker. Give each worker the current baseline, scope, applicable
guidance, expected output, and verification requirements.

Keep requirements, security decisions, concurrency decisions, and acceptance
judgment with the coordinator. Inspect material outputs and completed results.
A worker summary is not proof that a check passed.

Before reporting completion, compare the change with the coding rules. State
which checks completed, which were not run, and any unresolved requirements.
Report timeouts, aborted checks, and incomplete runs as failures. End the user
report with a confidence score from 0 to 100%.

## Pull requests

Use brief descriptions with these headings:

- Summary
- What changed
- Why it changed
- Tests
  - Updated
  - New

For documentation-only changes, state that application tests were not applicable
and list the documentation checks that actually ran.
