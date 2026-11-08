# Project documentation

Misanthrope is in pre-implementation planning. The guides below set contribution
requirements now. They do not establish that the application, sandbox, tests, or
automated checks already exist.

## Start here

1. Read the root [contributor guidance](../AGENTS.md).
2. Read the [coding rules](coding-rules.md) before code or test work.
3. Follow the [security rules](security-coding-rules.md) and
   [testing standards](testing-standards.md).
4. Check the [platform targets](platform-support.md) for platform-specific work.

## Document owners

| Document | Purpose |
| --- | --- |
| [Project overview](../README.md) | Product scope, proposed architecture, and roadmap |
| [Coding rules](coding-rules.md) | Rust design, ownership, errors, concurrency, and workflow |
| [Security coding rules](security-coding-rules.md) | Permission, sandbox, credential, Git, and UI boundaries |
| [Testing standards](testing-standards.md) | Test design, execution, and evidence requirements |
| [Platform support](platform-support.md) | Windows/Linux targets and the evidence needed for support |
| [Provider authentication](provider-authentication.md) | Required subscription/API profiles, provider evidence, and readiness gates |
| [Development tools](development-tools.md) | Installed/managed toolchains, runtime grants, outputs, caches, and qualification |
| [Agent instructions](agent-instructions.md) | Product priorities, visible SYSTEM prompt, user additions, and request manifests |
| [System prompt draft](../prompts/system.md) | Versioned initial base instructions for the coding assistant |
| [Context management](context-management.md) | Token-based compaction, cache classes, invalidation, and efficiency measures |
| [Skills](skills.md) | Settings, package validation, enablement, loading, and controlled scripts |
| [MCP support](mcp.md) | Server connections, transports, credentials, capabilities, and tool authorization |
| [Session storage](session-storage.md) | JSONL layout/schema, durable journals, archive/restore, and safe resume |
| [Memories](memories.md) | User-selected project/global scope, management, retrieval, and lifecycle |
| [Repository checks](repository-checks.md) | CodeRabbit, CodeQL, Sonar, credentials, and main protection |
| [ADR register](adr/index.md) | Lasting decisions, alternatives, and consequences |
| [Changelog](../CHANGELOG.md) | Changes awaiting release |

## Updates required with a change

| Change | Update |
| --- | --- |
| Any repository change | Changelog and the relevant owning guide |
| A coding convention | Coding rules; an ADR when the choice needs lasting rationale |
| A security property, permission, or trust boundary | Security coding rules and relevant tests |
| A test convention or verification command | Testing standards |
| Repository analysis, CI workflows, or branch rules | Repository checks and relevant testing/security guidance |
| A supported platform or prerequisite | Platform support |
| Provider login, subscription entitlement, or billing | Provider authentication and security rules |
| Tool discovery, runtime access, build environments, or build/package caches | Development tools and platform support |
| Prompt text, user additions, or instruction assembly | Agent instructions, prompt draft, and security rules |
| Model context, compaction, or LLM artifact caching | Context management, security rules, and testing standards |
| Skills, imports, enablement, or skill resources | Skills, agent instructions, and relevant security/testing guidance |
| MCP connections, tools, resources, prompts, or client capabilities | MCP, security rules, and testing standards |
| Session format, persistence, archives, restore, or resume | Session storage, security rules, and testing standards |
| Memory saving, scope, retrieval, deletion, or reuse | Memories, agent instructions, context management, and security/testing guidance |
| Product scope or the proposed architecture | Root README |
| Project implementation status or module guidance | Root AGENTS.md and relevant directory guidance |
| A lasting decision with a plausible alternative | New ADR and register entry |
| A new document | This index, or its owning sub-index |

Keep each rule in one owning guide and link to it elsewhere. Use the ADRs for
rationale rather than duplicating full policy text. Do not create a new document
for each implementation task or temporary investigation.

Use Markdown headings, fenced code blocks with language tags, and relative
links for repository files. Keep whitespace clean and end files with a newline.

## Current enforcement status

Repository review and analysis are configured in the repository-checks guide.
There is no Cargo manifest, Rust toolchain selection, frontend project, or
application test suite yet. Application formatting, linting, tests, dependency
checks, and release checks must be introduced with the implementation they check.
Record their actual scope and limitations when they are added.
