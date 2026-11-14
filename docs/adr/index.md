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
| [0014: Use the renamed application namespace without automatic migration](0014-use-the-renamed-application-namespace-without-automatic-migration.md) | Superseded by ADR-0020 | 2026-11-09 |
| [0015: Archive operation logs without consuming settings capacity](0015-archive-operation-logs-without-consuming-settings-capacity.md) | Accepted | 2026-11-09 |
| [0016: Add a session-owned tool runtime with session schema v2](0016-add-a-session-owned-tool-runtime-with-schema-v2.md) | Accepted | 2026-11-09 |
| [0017: Preview edits before enabling replacement](0017-preview-edits-before-enabling-replacement.md) | Accepted | 2026-11-09 |
| [0018: Define checkpointed replacement before enabling overwrites](0018-define-checkpointed-replacement-before-enabling-overwrites.md) | Accepted | 2026-11-09 |
| [0019: Validate replacement records before native storage](0019-validate-replacement-records-before-native-storage.md) | Accepted | 2026-11-09 |
| [0020: Rename the project to OMGCode](0020-rename-the-project-to-omgcode.md) | Accepted | 2026-11-10 |
| [0021: Store checkpoints with native private handles](0021-store-checkpoints-with-native-private-handles.md) | Accepted | 2026-11-10 |
| [0022: Publish one verified stage on Linux ext4](0022-publish-one-verified-stage-on-linux-ext4.md) | Accepted | 2026-11-10 |
| [0023: Store Windows checkpoints with verified ACLs](0023-store-windows-checkpoints-with-verified-acls.md) | Accepted | 2026-11-11 |
| [0024: Publish one verified stage on Windows NTFS](0024-publish-one-verified-stage-on-windows-ntfs.md) | Accepted | 2026-11-11 |
| [0025: Restore and delete checkpoints explicitly](0025-restore-and-delete-checkpoints-explicitly.md) | Accepted | 2026-11-11 |
| [0026: Offer read/list tools through a workspace preference](0026-offer-read-list-tools-through-a-workspace-preference.md) | Accepted | 2026-11-11 |
| [0027: Confine execution with native primitives, starting with a process-tree core](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md) | Accepted | 2026-11-11 |
| [0028: Confine Linux runs with Landlock and a socket filter](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md) | Accepted | 2026-11-11 |
| [0029: Confine Windows runs in a workspace AppContainer with recorded grants](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md) | Accepted | 2026-11-11 |
| [0030: Run trusted commands through recorded toolchain profiles](0030-run-trusted-commands-through-recorded-toolchain-profiles.md) | Accepted | 2026-11-11 |
| [0031: Limit whole run trees with job objects and delegated cgroups](0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md) | Accepted | 2026-11-12 |
| [0032: Allow outbound network for trusted runs through task_network](0032-allow-outbound-network-for-trusted-runs-through-task-network.md) | Accepted | 2026-11-13 |
| [0033: Bound run disk growth by sampling writable roots](0033-bound-run-disk-growth-by-sampling-writable-roots.md) | Accepted | 2026-11-13 |
| [0034: Gate package networking behind enforceable destination control](0034-gate-package-networking-behind-enforceable-destination-control.md) | Accepted | 2026-11-13 |
| [0035: Fetch packages through a trusted allowlisted cache](0035-fetch-packages-through-a-trusted-allowlisted-cache.md) | Accepted | 2026-11-13 |
| [0036: Expose the package fetch cache read-only to Linux runs](0036-expose-the-package-fetch-cache-read-only-to-linux-runs.md) | Superseded by ADR-0037 | 2026-11-14 |
| [0037: Use a per-workspace cache root and a scoped Windows grant](0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md) | Accepted | 2026-11-14 |
| [0038: Record provider profiles without credentials](0038-record-provider-profiles-without-credentials.md) | Accepted | 2026-11-14 |
| [0039: Gate future model tools with bound, single-use approval records](0039-gate-future-model-tools-with-bound-single-use-approval-records.md) | Accepted | 2026-11-14 |
| [0040: Gate the model command tool with approvals and the trusted runner](0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md) | Accepted | 2026-11-14 |
