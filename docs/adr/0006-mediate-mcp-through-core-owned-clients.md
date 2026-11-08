# 0006: Mediate MCP through core-owned clients

- Status: Accepted
- Date: 2026-11-08

## Context

The user requires MCP support. External servers can supply tools and context,
run local processes, or change remote systems. They can also supply malicious
instructions. A protocol connection does not enforce the development-root rule.

## Decision

Implement MCP client support in the Rust core for stdio and Streamable HTTP.
Use the current 2026-07-28 protocol as the design baseline, with older revisions
only through qualified compatibility paths. Keep server registration, enablement,
credentials, and per-operation authorization in core-owned state.

Launch local servers through the existing confined runner. Authorize remote
calls against explicit destination/account/tool scopes, with no claim of remote
filesystem containment. Treat resources, prompts, catalogs, and results as
untrusted context. Do not advertise client-driven work or UI capabilities before
their handlers and user authorization flows exist. See the [MCP contract](../mcp.md).

## Alternatives considered

- Provider-managed MCP alone varies by provider and authentication route, and
  can bypass consistent local controls. Use provider-independent host clients.
- Starting local servers through an SDK's unrestricted default process launcher
  creates a second execution boundary. Integrate with the controlled runner.
- Trusting read-only annotations or enabled connections as blanket permission
  lets servers define their own authority. Authorize operations in the core.
- Implementing every optional extension at once expands attack surface and
  review scope. Add capabilities through explicit contracts and verification.

## Consequences

SDK integration, protocol-version tests, safe authentication, and lifecycle
handling are required. Some servers will be incompatible with initial limits;
report that explicitly. Disconnection cannot prove rollback of remote effects.
No MCP implementation or compatibility result exists yet.

## References

- [Security coding rules](../security-coding-rules.md)
- [Development tools](../development-tools.md)
- [MCP specification](https://modelcontextprotocol.io/specification/2026-07-28)
