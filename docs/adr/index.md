# Architecture decision register

Record decisions whose rationale could otherwise be lost: a constraint,
a rejected plausible alternative, or a convention future code must follow.

Use the [template](template.md). Each record must include status and date.
Choose the next number above every existing record; never reuse a number,
renumber a record, or delete one to conceal a changed decision. Add each record
to this register in the same change.

Mark a changed decision superseded and link the replacing record. Update its
owning guide in the same change. A short-lived implementation detail with no
meaningful alternative does not need an ADR.

| Record | Status | Date |
| --- | --- | --- |
| [0000: Record architecture decisions](0000-record-architecture-decisions.md) | Accepted | 2026-11-08 |
| [0001: Adapt contributor rules to Rust](0001-adapt-contributor-rules-to-rust.md) | Accepted | 2026-11-08 |
| [0002: Require subscription authentication](0002-require-subscription-authentication.md) | Accepted | 2026-11-08 |
| [0003: Use scoped development toolchain profiles](0003-use-scoped-development-toolchain-profiles.md) | Accepted | 2026-11-08 |
| [0004: Require pull requests and repository analysis](0004-require-pull-requests-and-repository-analysis.md) | Accepted | 2026-11-08 |
| [0005: Keep agent context subordinate to runtime policy](0005-keep-agent-context-subordinate-to-runtime-policy.md) | Accepted | 2026-11-08 |
| [0006: Mediate MCP through core-owned clients](0006-mediate-mcp-through-core-owned-clients.md) | Accepted | 2026-11-08 |
| [0007: Store session journals as dated JSONL](0007-store-session-journals-as-dated-jsonl.md) | Accepted | 2026-11-08 |
| [0008: Compile the base prompt and separate user preferences](0008-compile-the-base-prompt-and-separate-user-preferences.md) | Accepted | 2026-11-08 |
| [0009: Require user-selected project and global memories](0009-require-user-selected-project-and-global-memories.md) | Accepted | 2026-11-08 |
| [0010: Build a local chat core before execution](0010-build-a-local-chat-core-before-execution.md) | Accepted | 2026-11-08 |
| [0011: Reserve terminal journal capacity](0011-reserve-terminal-journal-capacity.md) | Accepted | 2026-11-08 |
| [0012: Separate workspace settings from execution authority](0012-separate-workspace-settings-from-execution-authority.md) | Accepted | 2026-11-08 |
| [0013: Enforce file access with native handles and exclusive creation](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md) | Accepted | 2026-11-08 |
| [0014: Use the renamed application namespace without automatic migration](0014-use-the-renamed-application-namespace-without-automatic-migration.md) | Accepted | 2026-11-09 |
| [0015: Archive operation logs without consuming settings capacity](0015-archive-operation-logs-without-consuming-settings-capacity.md) | Accepted | 2026-11-09 |
| [0016: Add a session-owned tool runtime with session schema v2](0016-add-a-session-owned-tool-runtime-with-schema-v2.md) | Accepted | 2026-11-09 |
| [0017: Preview edits before enabling replacement](0017-preview-edits-before-enabling-replacement.md) | Accepted | 2026-11-09 |
