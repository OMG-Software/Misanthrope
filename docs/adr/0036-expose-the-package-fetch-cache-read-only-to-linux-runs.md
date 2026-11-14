# 0036: Expose the package fetch cache read-only to Linux runs

- Status: Superseded by ADR-0037
- Date: 2026-11-14

## Context

[ADR-0035](0035-fetch-packages-through-a-trusted-allowlisted-cache.md) adds a
trusted fetch into `<data-dir>/package-cache/<workspace-id>`. The confined run
does not read that cache yet, so a fetched package cannot be used by a build
without operator copying.

The cache is application storage. The project keeps settings and journals
outside the task's view. Exposing the whole data root is unacceptable; only the
per-workspace cache subtree is a candidate.

On Linux, Landlock can add a read-only path rule for that subtree, and the run
can receive a runner-owned variable naming it. On Windows, the AppContainer
needs an ACL grant to read application storage, and `workspace container-grant`
refuses paths that overlap application storage. A cross-platform cache root
would need a new operator setting and a read-execute container grant; that is
a larger design.

## Decision

On Linux, a trusted run exposes the workspace's fetched-package cache when it
exists:

1. The path is `<data-dir>/package-cache/<workspace-id>`. It must exist as a
   plain directory; a link or any other entry is not exposed.
2. Only that directory is added to the Landlock read-only rules. The parent
   `package-cache` directory, the rest of the data root, and every other
   workspace's cache remain outside the run's view.
3. The runner sets `OMGCODE_PACKAGE_CACHE` to the cache path. The variable is
   reserved against toolchain profiles, like `HOME` and `PATH`.
4. The run cannot write the cache. The rule is read-only, and the cache files
   remain private to the user.
5. `run_started` records `package_cache: true` when the run had this access.
   Runs without the cache, and journals written before 0.22.0, omit the field.
   A 0.22.0 journal that contains the field cannot be read by 0.21.0.
6. Tool-specific use is operator configuration. The package cache is a
   directory and a variable, not an automatic Cargo, npm, or pip cache
   integration.

On Windows, this increment does **not** expose the cache because no
unprivileged AppContainer path can read application storage without weakening
the grant rule. A Windows run keeps working without `OMGCODE_PACKAGE_CACHE`.
A later decision may add an operator-configured cache root with a read-execute
container grant.

## Alternatives considered

- **Expose the whole data root read-only.** Rejected: it would expose
  settings, journals, and other workspaces' caches.
- **Grant the AppContainer read access to application storage.** Rejected:
  it breaks the application-storage overlap rule and exposes more than the
  cache.
- **Write the cache into the workspace.** Rejected: the trusted fetch would
  bypass the replacement contract, and the run could rewrite the cache.
- **Configure a separate cache root outside application storage and grant it
  read-execute to the container.** Deferred: this is the cross-platform fix,
  but it needs a new setting, grant rules, and platform qualification.
- **Do not expose the cache on Linux until Windows is ready.** Rejected: the
  Linux control is enforceable now, and the gap is explicit.

## Consequences

- Linux runs can read fetched packages directly; Windows runs cannot yet.
- The run learns the cache path, and therefore the data-root prefix, through
  the variable. It still cannot list or read the parent or any sibling.
- The cache is read-only. There is no invalidation, eviction, cleanup, or
  freshness check; the operator manages it.
- The cache is private application storage, and this narrow subtree is a
  reviewed exception to keeping application storage outside the task view.
  Do not extend the exception to the parent, journals, settings, or other
  workspaces.
- Tests cover Linux read access, write denial, parent denial, the journal
  field, the reserved variable, and the Windows absence.

## References

- [Command execution](../execution.md#trusted-package-fetch)
- [Development tools](../development-tools.md)
- [Workspace settings](../workspace-permissions.md)
- [Security coding rules](../security-coding-rules.md#process-and-sandbox-boundary)
- [Testing standards](../testing-standards.md)
- [ADR-0035](0035-fetch-packages-through-a-trusted-allowlisted-cache.md)
- Superseded by [ADR-0037](0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md)
