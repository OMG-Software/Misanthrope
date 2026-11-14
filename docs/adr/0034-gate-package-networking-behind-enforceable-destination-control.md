# 0034: Gate package networking behind enforceable destination control

- Status: Accepted
- Date: 2026-11-13

## Context

The user selected a package-network profile as the next execution increment.
The acceptance case in [development tools](../development-tools.md) requires an
approved profile to reach its declared registry destinations and reject other
destinations.

Trusted runs have all-or-nothing network access today. `omgcode run --network`
with the `task_network` preference allows every destination on Linux and
internet destinations on Windows; everything else is denied. See
[ADR-0032](0032-allow-outbound-network-for-trusted-runs-through-task-network.md).

A destination-scoped profile needs a control that a hostile or compromised
package script cannot bypass. The available primitives do not provide one:

- **Linux Landlock** handles TCP bind and connect by **port**, not by address,
  from ABI 4. A rule cannot say "only `registry.example.com`". Seccomp's
  classic BPF can inspect syscall numbers and register values, not the
  `sockaddr` contents.
- **Windows AppContainer** capabilities are coarse. `internetClient` allows
  internet destinations; withholding it allows none, including loopback. There
  is no unprivileged destination filter. Windows Filtering Platform, firewall
  rules, and loopback exemptions need administrative setup.
- **Proxy environment variables and a loopback proxy** are advisory. A
  confined tree can ignore `HTTP_PROXY`, open its own socket, and connect
  directly. They cannot enforce a registry list.
- **Filesystem or NTFS quotas are irrelevant to networking.** Host firewall
  and nftables rules also need administrative setup.
- A user and network namespace design could isolate egress in principle, but
  unprivileged user namespaces are restricted on current Ubuntu, are not
  available on Windows, and add a large unsafe surface. A privileged helper is
  outside the prototype's unprivileged confinement model.

## Decision

Define the package-network profile as a **contract only** and gate any future
implementation:

1. A package-network profile is an operator-declared, revisioned list of
   registry origins: scheme (`https` in normal use; `http` only for a loopback
   test fixture), host, optional port, and an optional path prefix. It has no
   credentials, wildcards, user information, or redirects to other origins.
   It is configuration only and never authorizes execution.
2. No run may use a package-network profile in this phase. `omgcode run` gains
   no package-network flag, and `--network` remains all-or-nothing. A profile
   is not recorded as a run's authority or network mode.
3. A future implementation must pass all of these gates before it can be
   described as implemented:
   - the confined tree cannot reach any destination other than the declared
     origins or a trusted proxy, including direct IP, IPv6, UDP, DNS, loopback,
     alternate socket families, and paths that bypass the proxy;
   - the mechanism fails closed when it cannot be applied, and a platform that
     cannot enforce it refuses the profile rather than falling back to general
     network access;
   - the exact limits and bypass analysis are documented, and the mechanism is
     qualified on both supported hosts;
   - registry credentials remain a separate reviewed design and never enter a
     profile, run environment, journal, or model context;
   - acceptance tests prove allowed-origin success, undeclared-origin refusal,
     redirect refusal, credential refusal, offline fallback, and platform
     refusal.
4. Candidate mechanisms for a later decision are a trusted host-side fetch
   into a private cache with an offline run, an OS destination filter, or a
   Linux-only network-namespace relay. None is selected here. Each needs its
   own ADR.

## Alternatives considered

- **Advisory proxy plus environment variables.** Rejected: it is not a
  boundary; a hostile package script can connect directly.
- **Landlock port allowlist.** Rejected: it cannot name destinations, and any
  host on the allowed port remains reachable.
- **`internetClient` plus proxy variables on Windows.** Rejected for the same
  reason; the capability cannot be narrowed.
- **Host firewall, nftables, WFP, or loopback exemption.** Rejected for the
  prototype: administrative setup, per-host state, and an unqualified
  configuration surface.
- **Unprivileged user and network namespaces.** Not selected now: restricted
  on current Ubuntu, unavailable on Windows, and a large unsafe addition.
- **Trusted host-side fetch with an offline run.** Deferred to its own
  decision. It is enforceable but changes the feature from networked runs to
  cache population.

## Consequences

- No runtime capability is added, and the product status must not describe
  package networking or destination filtering as implemented.
- Version 0.20.0 records operator-declared profiles as configuration. That does
  not change this gate, and no run accepts one.
- Package downloads still require the all-or-nothing `task_network` and
  `--network` path, or an offline cache the operator prepares.
- The profile contract and the implementation gate are available for a future
  increment. Any implementation needs a new ADR, platform qualification, and
  acceptance tests.
- The project keeps its rule that a control is not called implemented until
  the runtime enforces it.

## References

- [Command execution](../execution.md#package-network-profile-contract-only)
- [Development tools](../development-tools.md)
- [Security coding rules](../security-coding-rules.md#provider-traffic-and-tool-calls)
- [Testing standards](../testing-standards.md)
- [ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
- [ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md)
- [ADR-0029](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md)
- [ADR-0032](0032-allow-outbound-network-for-trusted-runs-through-task-network.md)
