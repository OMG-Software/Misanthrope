# 0032: Allow outbound network for trusted runs through task_network

- Status: Accepted
- Date: 2026-11-13

## Context

[ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md) and
[ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
denied every IP socket to a confined run. Package downloads, such as
`cargo fetch`, `pip install`, and `npm install`, therefore failed. The
workspace `task_network` preference existed but enforced nothing. Increment
C6 adds outbound network access for trusted runs.

The user chose these options for C6:

- **Gating:** a run gets network access only when the workspace
  `task_network` preference is enabled and the run passes `--network`. The
  default stays no network.
- **Linux reach:** all IP destinations, with the exposure documented, rather
  than a port or address allowlist that Landlock and seccomp cannot express
  fully.
- **Windows capability:** `internetClient` only.
- **Resolver and certificate files:** a fixed read-only list chosen by the
  runner, not operator-added paths.

Facts observed during the work, on Windows 11 build 26300 and WSL 6.6
(Landlock ABI 3):

- A confined Linux program can reach only listed paths, so without
  `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, and the
  certificate stores it can neither resolve a name nor verify a certificate.
- Under WSL, `/etc/resolv.conf` is a link to `/mnt/wsl/resolv.conf`. Landlock
  rules cannot contain a link, so each path is resolved first.
- Landlock ABI 4 handles TCP bind and connect by port only. ABI 3 handles no
  network rights. Neither can filter by address.
- An AppContainer with `internetClient` reaches internet hosts but not
  loopback; `privateNetworkClientServer` is a separate capability.

## Decision

`Confinement` gains `network: NetworkAccess`, `Denied` by default or
`Internet`. The [execution guide](../execution.md#network-access) owns the
details.

- **Gating:** the core refuses `RunRequest.network` unless `task_network` is
  enabled, before anything is recorded. The run watcher checks the
  preference after each settings change and stops a networked run, recorded
  as `revoked`, when it is turned off. A run without network access ignores
  the preference.
- **Linux:** the seccomp filter allows `socket(AF_INET)` and
  `socket(AF_INET6)` of any type. Every other family keeps its rule;
  `socketpair` stays Unix-only and `io_uring` stays denied. On Landlock ABI 4
  and later the ruleset stops handling TCP connect but still handles TCP
  bind, so the tree cannot serve TCP. The runner adds the existing entries of
  a fixed list of resolver and certificate paths, as real paths, to the
  read-only rules.
- **Windows:** the AppContainer's `SECURITY_CAPABILITIES` carries the
  `internetClient` capability SID, enabled. No other capability is added.
- **Records:** `run_started` records `network` with the support paths the run
  could read. Earlier records without the field replay as runs without
  network access.

## Alternatives considered

- **Web ports only.** Allowing only TCP 80 and 443 through Landlock would not
  limit UDP, so DNS would still reach any port, and ABI 3 cannot express it
  at all. Many registries and Git hosts use other ports. Rejected for C6.
- **Private networks on Windows.** `privateNetworkClientServer` would also
  allow inbound connections and the local network. Rejected: package
  downloads need only the internet.
- **Operator-added support files.** Letting profiles add resolver or
  certificate paths would widen the trusted file set per run. The fixed list
  covers common distributions; profiles can still list their own roots.
- **Profile-level gating.** Recording network access on the toolchain profile
  would let every run of that profile reach the network. A per-run flag keeps
  the decision explicit.
- **A network namespace, proxy, or address filter.** These could limit
  destinations but need privileges, a helper service, or a dependency.
  Deferred to a later decision.

## Consequences

- A networked Linux run can reach every IP destination, including loopback
  services, the local network, and cloud metadata endpoints. A networked run
  on either platform can send any workspace data it can read to any host it
  can reach.
- On Landlock ABI 3, nothing stops a networked Linux run from listening on
  TCP. UDP bind is never limited.
- A networked Linux run can read the host's `/etc/hosts` and resolver
  configuration.
- Turning `task_network` off stops networked runs of the workspace.
- There is still no destination allowlist, registry credential path, or
  development-server networking.

## References

- [Execution contract](../execution.md#network-access)
- [Workspace settings](../workspace-permissions.md)
- [ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md)
- [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
- [ADR-0030](0030-run-trusted-commands-through-recorded-toolchain-profiles.md)
- Linux `Documentation/userspace-api/landlock.rst`: network rules from ABI 4
- Windows `SECURITY_CAPABILITIES` and `WinCapabilityInternetClientSid`
