# Development tools and toolchains

## Required scope

OMGCode's Rust implementation language does not limit the languages it can
help develop. The coding assistant must be able to discover and run the tools
a project needs, including:

| Tool family | Representative tools |
| --- | --- |
| Compilers, linkers, and SDKs | Rust, C/C++, native Windows toolchains, system SDKs |
| Formatters and linters | rustfmt, Clang tools, language-specific formatters |
| Interpreters and shells | Python, Node.js, Bash, PowerShell, other registered runtimes |
| Build systems | Cargo, Make, CMake, Ninja, Meson, MSBuild, project scripts |
| Package and environment managers | Project dependency managers and virtual environments |
| Testing and development support | Test runners, code generators, coverage tools, language servers |

These are required tool categories and profile candidates, not a claim that
every listed executable or version already works. Version 0.16.0 adds the
trusted operator command `omgcode run`. It runs one program under Linux
Landlock and seccomp rules or a Windows AppContainer, with an
operator-declared [toolchain profile](execution.md#toolchain-profiles).
Version 0.17.0 limits each run's memory, live processes, and CPU time; raise
the [tree limits](execution.md#tree-limits) for large builds. On Linux the
process limit counts threads, so a parallel compiler needs a higher limit
than its process count. Version 0.19.0 adds a sampled limit on net disk growth
for the writable roots; it is not a filesystem quota. No
model tool runs project tools. A model command tool can request one only
through the trusted `run` path and a consumed approval. Tool support must be
qualified on the actual
platform; the [execution guide](execution.md#known-limits) lists the
toolchains and versions that passed acceptance.

Provide a generic process runner and extensible toolchain registration.
Do not make useful development depend on a fixed list of approved command names.
Each command still needs the user's execution permission and an effective
sandbox profile.

## Tool sources

Support user-selected installed toolchains and managed toolchain environments.
An installed toolchain may need executables, helper programs, shared libraries,
SDK files, configuration, and machine-derived environment values. A single
executable-path grant is often insufficient.

Profiles are currently declared by the operator, with no discovery; see
[ADR-0030](adr/0030-run-trusted-commands-through-recorded-toolchain-profiles.md).
Any later discovery runs through a trusted host-side service. It proposes the
required resources for user selection; the model cannot approve new host
access.
Do not execute repository-controlled discovery hooks or version probes with
unrestricted host privileges. Run executable probes in confinement.

For an installed toolchain, expose only the required installation resources,
normally read-only. A managed environment can provide an isolated installation
or reproducible image. Choose the method that the platform and tool support;
do not assume that copying arbitrary installed SDKs preserves their behaviour
or distribution rights.

## Execution profiles

Version 0.16.0 profiles record:

- identity;
- platform;
- read-only roots;
- `PATH` entries;
- extra variables.

The runner owns the scratch and home locations. The run deadline and output
limits are per-run options. Network destinations, loader search paths, and
recorded versions are not implemented.

Each selected toolchain profile must eventually define:

- Profile identity, tool origin, native platform/architecture, and recorded
  executable locations and versions.
- Read-only executable, library, runtime, SDK, and approved configuration roots.
- A curated environment, executable search path, and loader search paths.
- The session workspace and working directory.
- Writable source/output paths, private temporary space, and scoped caches.
- Network destinations and the separate credential policy, if any.
- Process-tree, duration, memory, process-count, disk, and output limits.

Show the user the effective resource grants in settings. A profile is an
explicit runtime/toolchain exception to the workspace data boundary. It does
not grant access to unrelated projects or the user's general home directory.

Revalidate profiles after relevant tool installations or versions change.
Do not resolve a trusted tool through a project-controlled PATH entry, launcher,
configuration, or library search path. Project-built programs and their
libraries remain untrusted and run inside the task sandbox.

Read-only resources must stay read-only through alternate paths and aliases.
Refuse a profile if the backend cannot enforce its required boundaries.

## Build inputs, outputs, and caches

- Permit ordinary workspace build inputs and project scripts within the user's
  effective read/write and command permissions.
- Provide writable output directories, compiler intermediates, private temp
  space, virtual environments, and package/compiler caches. Direct these writes
  to explicit workspace or managed-scratch locations.
- Do not inherit the user's entire environment, shell startup files, credential
  files, package-manager settings, or general caches. Derive only non-secret
  tool setup values that the profile requires.
- Keep mutable caches isolated by workspace/session as needed. A shared cache
  needs a reviewed design for data leakage, integrity, concurrency, and cleanup.
- Treat build scripts, package lifecycle scripts, generators, test programs,
  formatters, and language servers as executable project/dependency code.
- Apply containment to all helper programs and descendants, including programs
  invoked by a compiler, interpreter, or build-system driver.
- Return failures with the missing tool/resource and a usable diagnostic.
  Do not retry the command outside confinement to make it succeed.

## Installation, downloads, and services

Tool discovery and tool execution are separate from installation or updates.
Do not let a normal task modify a host compiler, global package installation,
system configuration, or another workspace's environment.

User-requested dependency installation can write into its authorized workspace
environment or managed scratch. Changing a managed toolchain requires a separate
approved operation. Host installation remains an explicit user/admin action.

Package downloads use task-network permission, never the model connection's
permission. Since 0.18.0 a trusted run gets outbound networking only with the
workspace `task_network` preference and its `--network` flag; see
[network access](execution.md#network-access). There is no destination
allowlist. The
[package-network profile](execution.md#package-network-profile-contract-only)
is recorded as configuration only (ADR-0034): `workspace
package-profile-add/show/remove` store registry origins, but no run accepts a
profile, and a proxy or proxy environment variables are not a boundary. The
trusted `packages fetch` command contacts only declared origins and writes
into the selected per-workspace cache root. A run can read only that root
through `OMGCODE_PACKAGE_CACHE`; Windows needs a read-execute container grant,
and Linux falls back to application storage when no root is selected.
Tool-specific
configuration to use that directory is operator work. Registry credentials
require a narrow reviewed credential path. Offline tasks
must work with available dependencies or explain which download is needed.

Development servers, debuggers, license servers, hardware access, and other
services can need extra resources. Treat each as an explicit capability rather
than granting broad host networking, device access, or administration privileges.
A preview needs controlled port forwarding and an unprivileged viewer.

## Platform qualification and acceptance

The pinned Rust toolchain in [coding-rules.md](coding-rules.md) builds OMGCode
itself. Toolchain profiles for user projects have their own requirements.

Validate representative Rust, C/C++, Python, and JavaScript workflows on Windows
and Linux, including a native Windows compiler/SDK profile. Establish the exact
supported versions through those tests. Do not infer that Linux execution or
WSL verifies native Windows tools.

A cross-compilation profile is a separate tested configuration. An explicitly
selected VM/container backend may provide tools, but must not expose a host
administration socket or silently change the target environment.

Acceptance checks must demonstrate discovery, a real compile/build, formatting,
script execution, tests, scoped outputs/caches, subordinate tool execution,
cancellation, denied host-data access, and disabled networking. Include missing
dependencies, spaces and native path forms, stale tool profiles, concurrent
builds, disk exhaustion, and hostile build/package scripts.

The trusted `packages fetch` command is covered by core and CLI tests. The
run-side package-network acceptance case still cannot run: no run accepts a
profile. A run can read the selected cache root, but run-side destination
scoping is still missing. A future implementation must prove
that an approved profile reaches its declared registry destinations and
rejects every other destination, including direct IP, IPv6, UDP, DNS, and
proxy-bypass paths. Model-provider connectivity alone must not enable package
or general task traffic.

Record the selected profile with task results so failures are reproducible.
Do not claim general support from locating an executable or printing its version.

The 0.16.0 acceptance tests cover these on both platforms:

- builds, tests, and formatting;
- script and subordinate execution;
- scoped outputs and caches;
- denied host data, writes outside the workspace, and loopback connections;
- missing dependencies, paths with spaces, and stale profiles;
- concurrent runs and deadlines;
- cancellation, on Linux.

They are listed in the [execution guide](execution.md#acceptance). Not covered
yet:

- disk exhaustion under concurrent writers, and inode, directory-entry, or
  metadata exhaustion; the sampled disk limit is tested with ordinary file
  growth;
- package downloads through a run's `--network`, and rejection of undeclared
  destinations, because there is no destination allowlist; the trusted
  `packages fetch` path is separate and the run can read only the selected
  cache root, not use run-side package networking;
- hostile package scripts beyond the hostile programs.

The Java case has not run on either host. The registry case also remains open.

See the [security rules](security-coding-rules.md),
[platform targets](platform-support.md), and
[toolchain decision](adr/0003-use-scoped-development-toolchain-profiles.md).
