# 0035: Fetch packages through a trusted allowlisted cache

- Status: Accepted
- Date: 2026-11-13

## Context

[ADR-0034](0034-gate-package-networking-behind-enforceable-destination-control.md)
gates destination-scoped package networking on an enforceable mechanism. The
current primitives cannot filter destinations for a confined tree:
Landlock filters TCP by port, classic seccomp cannot read `sockaddr`, and
AppContainer capabilities cannot name hosts. An advisory proxy is bypassable.

Version 0.20.0 records operator-declared package-network profiles but no run
accepts one. The next increment is an enforceable download path.

A trusted host-side fetch is enforceable without a new kernel primitive: the
trusted CLI performs the request in the operator's context, and the confined
run keeps no network access. The cache is private application storage.

## Decision

Add the trusted operator command:

```text
omgcode packages fetch <workspace-id> --profile <name> --url <url>
    --name <cache-file> [--max-bytes <1-1073741824>]
    [--timeout-secs <1-600>]
```

Rules:

- **Caller:** trusted CLI only. No model tool, run, or journal record can
  invoke it. The workspace must be registered and its live `task_network`
  preference must be enabled. A profile is selected by name and its recorded
  revision is audited.
- **Destination:** the URL is absolute and must match a recorded origin's
  scheme, host, port, and path prefix. Normal use is `https`; `http` is
  accepted only for a loopback test fixture. Credentials, queries, fragments,
  and wildcards are refused. Redirects are never followed; a non-2xx response
  is a failure.
- **Transport:** TLS uses the operating-system trust store through
  `reqwest`'s `native-tls` feature. Certificate validation is never disabled.
  The client ignores proxy environment variables and retries nothing.
- **Bounds:** one GET, a deadline of 1 to 600 seconds, and a response body of
  1 byte to 1 GiB. The body is streamed, hashed with SHA-256, and stopped as
  soon as it passes the byte limit.
- **Cache:** `<data-dir>/package-cache/<workspace-id>/<cache-file>`. The name
  is one safe file name; directories request 0700 and files 0600 on Unix. A
  `.partial-<fetch-id>` file is synced and renamed into place. An existing
  cache name is never overwritten. The cache makes no power-loss claim; a
  failed fetch may leave a partial file that the operator can inspect.
- **Audit:** the control journal records `package_fetch_started` before the
  request and `package_fetch_finished` after it. Both preserve the settings
  revision. They record the profile identity and revision, a SHA-256 of the
  URL (never its text), the cache name, limits, HTTP status, byte count, and
  content digest. A failure records a controlled reason. An abandoned fetch
  leaves its start record with no finish; nothing retries automatically.
- **Run boundary:** the confined run still has no network access and does not
  yet read the cache. Exposing the cache to a run is a separate increment and
  needs its own review of read-only confinement and tool configuration.

## Alternatives considered

- **Advisory proxy or proxy environment variables.** Rejected: the tree can
  connect directly.
- **An OS destination filter or Linux network-namespace relay.** Deferred:
  no unprivileged cross-platform primitive; the namespace design needs user
  namespaces and is Linux-only.
- **Fetch from inside the confined run.** Rejected: it would restore general
  network access without a destination filter.
- **Write the cache into the workspace for immediate tool use.** Rejected for
  this increment: the trusted command would bypass the workspace replacement
  contract, and a workspace-writable cache is not a protected cache. A
  private cache plus a later read-only exposure keeps the boundary visible.
- **A new dedicated package journal.** Rejected for now: the control journal
  is already the bounded settings/audit log, and fetch events preserve its
  settings revision. A separate journal would add another recovery state
  machine without adding authority.

## Consequences

- Package downloads have an enforceable allowlisted path, and the run remains
  offline. The fetch is not a general host shell or a network relay.
- The cache is not yet available to runs or toolchains. The operator must
  inspect or import it until the exposure increment lands.
- Registry credentials are still unsupported. The URL scheme, host, port, and
  path are the only authorization; a credential path needs a separate design.
- `reqwest` gains the `native-tls` feature and its dependency tree. TLS uses
  the system trust store. The tests use loopback HTTP; they do not qualify a
  live HTTPS registry.
- The cache is private to the user. A hostile same-user process can change it,
  and the cache is not a conflict-free or power-loss-qualified store.

## References

- [Command execution](../execution.md#package-network-profile-contract-only)
- [Development tools](../development-tools.md)
- [Workspace settings](../workspace-permissions.md)
- [Security coding rules](../security-coding-rules.md#provider-traffic-and-tool-calls)
- [Testing standards](../testing-standards.md)
- [ADR-0034](0034-gate-package-networking-behind-enforceable-destination-control.md)
