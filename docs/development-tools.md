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
every listed executable or version already works. The platform crate has an
unexposed process-tree runner core with no confinement; see
[command execution](execution.md). No command runs project tools yet. Tool
support must be qualified on the actual platform.

Provide a generic process runner and extensible toolchain registration.
Do not make useful development depend on a fixed list of approved command names.
Each command still needs the user's execution permission and an effective
sandbox profile.

## Tool sources

Support user-selected installed toolchains and managed toolchain environments.
An installed toolchain may need executables, helper programs, shared libraries,
SDK files, configuration, and machine-derived environment values. A single
executable-path grant is often insufficient.

Discovery runs through a trusted host-side service. It proposes the required
resources for user selection; the model cannot approve new host access.
Do not execute repository-controlled discovery hooks or version probes with
unrestricted host privileges. Run executable probes in confinement.

For an installed toolchain, expose only the required installation resources,
normally read-only. A managed environment can provide an isolated installation
or reproducible image. Choose the method that the platform and tool support;
do not assume that copying arbitrary installed SDKs preserves their behaviour
or distribution rights.

## Execution profiles

Each selected toolchain profile must define:

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

Package downloads use task/package-network permissions, never the model
connection's permission. Registry credentials require a narrow reviewed
credential path. Offline tasks must work with available dependencies or explain
which download is needed.

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

Also prove the allowed case: an approved package-network profile can reach its
declared registry destinations and rejects other destinations. Model-provider
connectivity alone must not enable that package or general task traffic.

Record the selected profile with task results so failures are reproducible.
Do not claim general support from locating an executable or printing its version.

See the [security rules](security-coding-rules.md),
[platform targets](platform-support.md), and
[toolchain decision](adr/0003-use-scoped-development-toolchain-profiles.md).
