# 0003: Use scoped development toolchain profiles

- Status: Accepted
- Date: 2026-11-08

## Context

The project owner requires access to the many tools used for software
development. Compilers and build systems need helper binaries, libraries, SDKs,
environment values, and writable build/cache directories. Workspace-only data
access does not by itself describe those resources.

## Decision

Support installed and managed toolchains through explicit profiles. The
[development tools guide](../development-tools.md) owns their resource and
acceptance contract.

Keep host toolchain inputs read-only where possible and declare every writable
output or cache resource. Use a generic sandboxed runner; all scripts and
descendants keep the effective restrictions. Toolchain registration cannot grant
general host access or model-provider network permissions to tasks.

OMGCode's own Rust toolchain and the user's project toolchains are separate
configurations. Qualify profiles on native Windows and Linux.

## Alternatives considered

- Expose the host PATH, home, and all tools: this also exposes unrelated data,
  mutable configuration, credentials, and services.
- Permit only a short fixed command list: legitimate compilers and build drivers
  launch other programs and need broader development workflows.
- Always copy or bundle every toolchain: toolchains differ in dependencies,
  installation assumptions, size, and distribution constraints.
- Retry outside the sandbox when a tool fails: this defeats the containment
  contract at the moment untrusted build code requests more access.

## Consequences

Profiles add setup and compatibility work. The UI must explain missing resources
and effective grants. A successful build must not imply unrestricted host
execution. No discovery or execution backend is implemented yet.

## References

- [Development tool requirements](../development-tools.md)
- [Security rules](../security-coding-rules.md)
