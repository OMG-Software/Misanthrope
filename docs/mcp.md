# Model Context Protocol support

This is a planned contract. No MCP client, settings page, or compatibility tests
exist yet. OrangeCode will act as an MCP host with Rust-owned clients for
external servers. Exposing OrangeCode itself as an MCP server is a separate
future capability, not required by this initial client design.

## Initial scope

Support local stdio servers and remote Streamable HTTP servers. Provide tool
discovery and calls, resource listing/reading through a context picker, and
explicit selection of server-provided prompt templates. Keep these integrations
independent of the selected LLM provider; adapt validated tools through the
provider's supported calling interface. Report incompatible schemas/capabilities
instead of silently changing their meaning.

Use the [2026-07-28 specification](https://modelcontextprotocol.io/specification/2026-07-28)
as the current design baseline. It uses stateless requests with protocol version
and client capabilities in request metadata. Do not require an initialization
handshake or protocol session ID for this version. Discovery and version
selection must follow the chosen protocol revision. Support older revisions only
through explicitly tested compatibility paths; never downgrade authentication
or containment to make an old server connect.

Select and pin a compatible Rust SDK with the implementation. Evaluate the
[official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) behind a
narrow adapter. Its availability does not prove containment or compatibility
with every server. The SDK must not spawn a server outside the controlled runner.

## Settings and permission controls

Provide an MCP settings page with Add, Remove, Enable, Disable, Connect, and
Disconnect controls. Show server identity, source, transport, executable/arguments
or endpoint, authentication status, selected protocol, capabilities, exposed
tools, and errors. Support application registrations with workspace/session
enablement scopes. Keep the registry outside task-writable storage.

New registrations remain disabled until the user enables them. Importing a
configuration is not permission to start a program, install a package, contact
a destination, or authorize a tool. Repository files, skills, server messages,
and model output cannot enable servers or change connection settings.

Permission settings must distinguish local server execution, remote MCP
networking, resource reads, and each server's tools. Give tools stable identities
qualified by the server registration, not just an untrusted display name. Show
the effective scope and any per-call approval. Enabling a connection grants no
blanket permission to every advertised operation. Reuse valid user grants within
their scope; do not ask again solely because MCP made another request.

Bind each call to the current session/workspace, server identity, account,
protocol and tool-schema revision, arguments, policy revision, and grant lifetime.
Revalidate before dispatch. Changed catalogs, identities, destinations, accounts,
or relevant settings invalidate affected pending requests, grants, and caches.
Treat annotations such as read-only or non-destructive as untrusted hints.
Unknown effects require a scoped user decision rather than automatic trust.

## Local servers

Launch stdio servers through the [development-tool runner](development-tools.md).
Declare the executable, runtime dependencies, working directory, environment,
filesystem resources, networking, limits, and process supervision. Prefer an
installed pinned executable. A command such as a package runner must not install
or update software implicitly during connection.

Apply restrictions before the server starts and to all descendants. Do not
inherit general host PATH, home configuration, secret stores, or credentials.
Supply only required configuration and separately selected credentials for that
server. Keep credentials out of arguments, transcripts, model context, and logs.
An authorized server may receive its own required credential through an explicit
profile; this does not authorize unrelated tasks to read or inherit it.

Enforce the stdio JSON-RPC channel and message-size limits. Keep diagnostic
stderr separate from protocol stdout. Stderr text alone is not a failed call.
Bound startup, output, concurrency, execution time, and restart attempts. A
restart must not automatically repeat an operation with an uncertain outcome.
Use session-scoped workers initially so one session cannot inherit another's
workspace grants or pending calls. Observe descendant cleanup on shutdown.

## Remote servers and authentication

Use a core-owned HTTP client with certificate-validated HTTPS for remote services.
An explicitly configured loopback HTTP development server is a separate profile;
it must not broaden access to host-local or private-network destinations.
Restrict destinations, redirects, DNS resolution, and authorization metadata
fetches. Do not fetch arbitrary URLs from tool results or resource contents.

Support the MCP HTTP authorization flow when required by the server. Validate
the protected resource, authorization issuer, intended audience, state, PKCE,
redirect binding, and returned identity. Bind stored credentials to their server,
issuer, account, and scope. Keep them in the credential service, separate from
LLM and Git credentials. Never pass an unrelated provider token through to a
server or disclose credentials after a destination change.

Support JSON responses and request-scoped SSE according to the selected
Streamable HTTP revision. Validate protocol routing headers derived from
untrusted names/arguments. Handle disconnection, malformed streams, bounded
retries, and cancellation. A network disconnect or cancellation request does
not prove that a remote write stopped or rolled back.

A remote server executes under its own service permissions. OrangeCode's
development-root boundary cannot contain that server's remote filesystem or
external effects. Disclose the selected account and delegated scope, and
authorize calls accordingly. Do not describe a successful MCP connection as
proof of remote containment.

## Content, capabilities, and lifecycle

Treat server descriptions, schemas, resources, prompts, diagnostics, and results
as untrusted content. Label their origin in the context inspector and journal.
Selecting an MCP prompt creates user-selected task context; it cannot replace
the base prompt, trusted custom instructions, or permission policy. Resource
URIs are server identifiers, not authority to open matching host paths.

Advertise only implemented client capabilities. Initially leave model sampling,
elicitation, roots, arbitrary UI extensions, and server-triggered host actions
disabled. Reject unsupported multi-round-trip input requests with a defined
protocol error; do not answer consent questions on the user's behalf. Future
support needs a reviewed user flow and the same operation-level authorization.

Keep server catalogs deterministic where the protocol permits caching. Key
cached metadata by server/account, authorized scopes, protocol, and content
revision. Load only enabled tools and selected resources/prompts into context.
Record their versions and actual usage with the
[instruction manifest](agent-instructions.md) and [session journal](session-storage.md).
Apply the [context/cache rules](context-management.md) to invalidation.

Disconnect or disable blocks new calls, invalidates dependent context/grants,
and cancels affected work. Observe local cleanup before reporting completion.
Keep uncertain remote outcomes visible and reconcile them before a retry. Do
not stop unrelated sessions. Replaying session history must not reconnect
servers or repeat calls. New user work may connect only under current settings
and authorization.

## Acceptance requirements

On Windows and Linux, test stdio and HTTP, supported/unsupported protocol
revisions, JSON and SSE, pagination, schema errors, malformed/oversized messages,
header injection, poisoned catalogs/results, prompt injection, identity/scope
changes, SSRF and redirect handling, OAuth callback binding, denied calls,
credential separation, process escape attempts, restart/cancellation, disable
during work, uncertain remote writes, and archive/resume without call replay.
Verify the actual execution/disclosure boundary, not server annotations.

## Primary references

- [Stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)
- [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [HTTP authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)
- [Tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- [Resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)
- [Security guidance](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices)

Protocol support, authentication, and extensions are separate capabilities.
Recheck the current contracts and qualify the selected SDK before implementation.
