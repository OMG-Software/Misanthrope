# 0037: Use a per-workspace cache root and a scoped Windows grant

- Status: Accepted
- Date: 2026-11-14

## Context

[ADR-0036](0036-expose-the-package-fetch-cache-read-only-to-linux-runs.md)
exposed the application-storage package cache to Linux runs. Windows was left
unexposed because the AppContainer needs an ACL grant to read application
storage, and container grants refuse that storage.

A configured cache root outside application storage solves that, but a single
global root has two problems on Windows:

- Granting the root to a workspace's container lets that container read every
  workspace's cache beneath it.
- Granting only the per-workspace directory leaves the parent unreadable, and
  `cmd` and similar tools can fail to resolve the path. Testing showed that the
  AppContainer could read a cache directly under a top-level granted root but
  not one beneath the user's private temporary directory.

The cache root therefore needs to be selected per workspace, and the grant
must cover that workspace's root.

## Decision

1. `workspace set-package-cache <workspace-id> <dir> --revision <n>` records one
   cache root for one workspace. The directory must exist, must not overlap
   application storage, must not overlap the development root, and must not
   overlap another workspace's cache root. `workspace
   clear-package-cache <workspace-id> --revision <n>` removes it. Both use the
   settings revision; the journal events are `package_cache_root_set` and
   `package_cache_root_cleared`.
2. `packages fetch` writes into the selected workspace root when one is set.
   Without one, Linux keeps the application-storage fallback
   `<data-dir>/package-cache/<workspace-id>`; Windows has no fallback.
3. A run uses the selected root when it exists as a plain directory:
   - **Linux:** the directory is added read-only and named by
     `OMGCODE_PACKAGE_CACHE`.
   - **Windows:** the directory is added read-only and named by
     `OMGCODE_PACKAGE_CACHE`. The workspace's container must hold a
     read-execute grant on that directory, or the run fails before it starts.
     The grant is scoped to that workspace's root; other workspaces use
     separate roots. The root's ancestors must be traversable by the
     container; use a dedicated top-level directory rather than a user's
     private temporary tree.
4. Only the selected directory is exposed. The parent, application storage,
   journals, settings, and other workspaces' roots stay outside the run's
   view.
5. `run_started` records `package_cache: true` when the run had access. The
   field remains optional; runs without a cache omit it.
6. The cache is a directory of fetched files. There is no eviction,
   invalidation, freshness check, or power-loss claim. Changing the root does
   not move existing files; the operator manages both locations and revokes
   any grant on an old root.

## Alternatives considered

- **One global cache root.** Rejected: a Windows grant on the root would
  expose every workspace's cache to each container.
- **Grant each per-workspace directory beneath one global root.** Rejected:
  the parent is unreadable to the container, and path resolution failed in
  testing.
- **Grant application storage to the AppContainer.** Rejected: it breaks the
  storage overlap rule and exposes journals and settings.
- **Write the cache into the workspace.** Rejected: the trusted fetch would
  bypass the replacement contract, and the run could rewrite the cache.
- **Keep the Linux-only exposure.** Rejected now that a scoped cross-platform
  design is available.

## Consequences

- Windows can expose a fetched cache when the operator selects a reachable
  root and grants the container read-execute on it. The run fails rather than
  silently omitting the cache when the grant is missing.
- The operator has one more per-workspace setting and must manage the
  container grant and the directory. Separate roots preserve cross-workspace
  isolation; overlapping roots are refused.
- The Linux application-storage fallback remains for compatibility. A
  configured root takes precedence; a stale configured root is not exposed and
  the fetch refuses it.
- Tests cover per-workspace set/clear/replay, overlap refusals, the configured
  fetch location, Linux read/write/parent denial, Windows access with a grant,
  and Windows refusal without one.

## References

- [Command execution](../execution.md#trusted-package-fetch)
- [Workspace settings](../workspace-permissions.md)
- [Development tools](../development-tools.md)
- [Security coding rules](../security-coding-rules.md#process-and-sandbox-boundary)
- [Testing standards](../testing-standards.md)
- [ADR-0036](0036-expose-the-package-fetch-cache-read-only-to-linux-runs.md)
