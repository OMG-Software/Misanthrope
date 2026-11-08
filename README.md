# Misanthrope

**A Rust-based coding assistant harness for Windows and Linux.**

Misanthrope is a planned desktop application that connects language models to
controlled development tools. It will bring model connections, workspace
permissions, Git workflows, terminals, and coding sessions into one interface.

> **Status: planning and pre-implementation.** This repository currently contains
> the initial project files. The features below are planned. There is no runnable
> application, build procedure, or verified sandbox implementation yet.

## Goals

1. **Secure operation:** Enforce the user's permissions and contain development
   tools on Windows and Linux.
2. **Prompt injection defence:** Keep untrusted content from granting authority.
   Use runtime controls, source tracking, and adversarial tests alongside the prompt.
3. **Efficiency:** Reduce unnecessary model calls, repeated context processing,
   and tool work while preserving correctness and security.

Security takes priority when an optimization would weaken these boundaries.

## Planned features

- **Multiple model providers:** OpenAI, Anthropic / Claude, Ollama Local,
  Ollama Cloud, OpenRouter, and LM Studio.
- **Subscription authentication:** Required ChatGPT and Claude subscription
  connections, alongside separate API-key connections. Provider eligibility
  and approval remain implementation gates.
- **Development tools:** Compilers, linkers, SDKs, formatters, linters, script
  interpreters, build systems, package managers, and test tools.
- **Permission settings:** Toggle switches for file access, command execution,
  background tasks, network access, and Git operations, with scoped approvals
  where required.
- **Development root:** A user-selected root that sets the maximum boundary for
  host development data. Sessions will normally have read and write access to
  their active workspace within that root. Other host folders will be denied
  by default.
- **Sandboxed tools:** File operations, terminals, and background tasks governed
  by the same permission policy and enforced with operating-system controls.
- **Git workflows:** Status, diffs, staging, commits, branches, worktrees, fetch,
  pull, and push, with controlled remote access and credential handling.
- **Development panels:** Terminal tabs, background task status and logs, and
  file and hunk diffs.
- **Persistent sessions:** New and resumable sessions in a tree grouped by
  working folder, with search, sorting, and session status. Save versioned JSONL
  journals at `sessions/<year>/<month>/<date>/misanthrope-<time>-<guid>.jsonl`.
- **Session archives:** Archive and restore conversations without deleting or
  moving their transcripts. See [session storage](docs/session-storage.md).
- **MCP support:** Connect local stdio and remote Streamable HTTP servers for
  tools, resources, and prompt templates through core-owned permissions. See
  [MCP support](docs/mcp.md).
- **Visible SYSTEM prompt:** Inspect a versioned base prompt compiled into the
  Rust application and shown read-only. Add separate user preferences at
  application, workspace, and session scope. See the
  [initial prompt draft](prompts/system.md) and [instruction design](docs/agent-instructions.md).
- **Automatic compaction:** Configure a trigger in tokens; 200k means 200,000
  tokens. Show any earlier trigger required by the selected model's safe budget.
- **Caching:** Reuse validated artifacts and supported provider prompt caches,
  with scoped storage, invalidation, usage reporting, and clear controls.
- **Skills settings:** Add, remove, enable, and disable versioned skill packages.
  Skills use the same permission policy and controlled tools as other work.
- **Project and global memories:** Save reusable facts or preferences with an
  explicit This project or Global choice. Review, edit, delete, or disable
  entries and control their use in each session. See [memories](docs/memories.md).
- **Change review and recovery:** Session change tracking, checkpoints, and
  worktrees to help review and recover changes.

## Model providers

| Connection profile | Planned connection |
| --- | --- |
| OpenAI API | Responses API with API-key authentication |
| ChatGPT subscription | Provider-supported sign-in and ChatGPT plan usage |
| Anthropic API | Claude Messages API with API-key authentication |
| Claude subscription | Required subscription sign-in; provider approval and route unresolved |
| Ollama Local | Local Ollama API |
| Ollama Cloud | Authenticated cloud API |
| OpenRouter | OpenRouter API with explicit upstream routing controls |
| LM Studio | Configured local OpenAI-compatible API, with server-token authentication where enabled |

API-key and subscription connections are distinct authentication and billing
profiles. Subscription-backed usage for both ChatGPT and Claude is required for
the first release. OpenAI documents a local/open-source integration; Anthropic
currently requires approval for third-party subscription integration. See the
[authentication requirements](docs/provider-authentication.md) for the evidence,
open dependency, and acceptance checks.

Provider support will depend on the capabilities of the selected model and
endpoint. Tool calling, streaming, context limits, and usage reporting will remain
explicit rather than being assumed to work identically across providers.

## Proposed architecture

- **Rust core:** Session management, agent execution, provider adapters,
  authentication, permissions, file access, toolchain profiles, Git operations,
  task supervision, prompt assembly, context management, caching, skills, MCP,
  and scoped memories.
- **Desktop interface:** Tauri with a bundled TypeScript web interface.
- **Execution backends:** Separate Windows and Linux sandbox implementations.
- **Local storage:** Versioned JSONL session journals, archive/restore events,
  a rebuildable folder/search index, settings, scoped memories, and referenced
  task artifacts.
- **Credential handling:** Operating-system credential storage, accessed through
  narrow services in the Rust core.

The interface will use a limited command interface to the Rust core. Provider
keys will stay out of frontend state, repository files, and task environments.

## Development tools

Misanthrope must work with projects in multiple languages. It will support
user-selected installed tools and managed toolchain environments through a
generic sandboxed runner. Tool profiles will include the required executable,
runtime, library, and SDK resources, plus scoped writable build outputs, caches,
and temporary space.

Build scripts and all child programs remain subject to the effective sandbox.
Toolchain access does not grant access to unrelated host data. See
[development tools and toolchains](docs/development-tools.md).

## Permission and sandbox design

Misanthrope will own its permission policy and sandbox lifecycle. The design will
use operating-system isolation to enforce restrictions on files, networking,
processes, and host services.

Linux isolation will evaluate namespaces, Landlock, seccomp, and resource limits.
Windows isolation will evaluate AppContainer, restricted process permissions,
filesystem grants, and Job Objects. Both backends must pass containment and
development-tool compatibility tests before release.

Development tools also need runtime files and temporary storage. These will be
declared sandbox resources, with private scratch space and controlled toolchain
access. The sandbox will not automatically expose the user's home directory,
credentials, or unrelated projects. If required isolation is unavailable,
affected operations will be refused.

Model connections, Git connections, MCP connections, and general task networking
will have separate controls. Sending source context to a cloud model is a data
transfer even when terminal networking is disabled. A local Ollama endpoint can also route
requests to cloud models, so local and cloud execution must remain distinct.

Prompt injection can still cause harmful actions within permissions the user
grants. Write access permits damage inside that writable workspace. The design
aims to contain access and provide review and recovery; it does not promise to
prevent every malicious instruction.

Prompts, custom instructions, memories, skills, summaries, and cached artifacts
cannot grant permissions. See [context management](docs/context-management.md) and
[skills](docs/skills.md) for their trust boundaries and acceptance requirements.

## Roadmap

1. Define the threat model, workspace boundary, and permission contract.
2. Resolve subscription integration eligibility and validate both login/usage paths.
3. Prove Windows/Linux isolation, representative toolchains, and process cleanup.
4. Build the Rust core, desktop interface, JSONL persistence, session tree, and
   archive/restore flow with crash recovery.
5. Add provider adapters, streamed conversations, and validated tool calls.
6. Add visible instructions, token accounting, compaction, safe caching, skills,
   and project/global memories with boundary tests and efficiency measurements.
7. Add scoped file tools, sandboxed terminals, background tasks, and MCP clients.
8. Add mediated Git workflows, diff review, checkpoints, and crash recovery.
9. Verify the full workflow on both platforms and prepare installers and
   documentation.

## Documentation

Start with the [documentation index](docs/README.md). Contributors and coding
agents must follow the [coding rules](docs/coding-rules.md) before code or test
work.

## Contributing

The project is in its planning stage. Use
[GitHub Issues](https://github.com/OMG-Software/Misanthrope/issues) to discuss
requirements, design proposals, and gaps in the plan.

Build instructions will be added when implementation begins. See the
[contributor guidance](AGENTS.md) for the current workflow.

## Repository checks

Main updates require pull requests. CodeRabbit reviews pull requests, CodeQL
analyzes workflow files and available application languages, and Sonar uses
CI-based analysis. See [repository checks](docs/repository-checks.md) for the
configured scope, credential setup, and current limitations.

## License

Misanthrope is licensed under the [Apache License 2.0](LICENSE).
