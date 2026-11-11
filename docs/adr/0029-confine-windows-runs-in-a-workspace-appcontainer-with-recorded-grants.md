# 0029: Confine Windows runs in a workspace AppContainer with recorded grants

- Status: Accepted
- Date: 2026-11-11

## Context

[ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
selected AppContainer inside a Job Object for Windows confinement.
[ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md) added
`run_confined` on Linux and left Windows refusing it. C3 must provide a
per-workspace container identity, recorded and reversible access entries on the
workspace and toolchain roots, no network capability, and denied host data.

An AppContainer reaches a file only when the file's DACL grants the container
SID, or every application package, the requested access. Unlike Landlock, the
rules are therefore ACL entries on the objects themselves. They outlive the run
and are visible to other programs.

The user chose these options for C3:

- **ACL lifetime:** persistent grants, recorded in the control journal and
  removed only by an explicit revocation.
- **Read-only roots:** a read-execute entry for the container SID on each
  listed toolchain root, recorded and revocable like a workspace grant.
- **Host services:** refused on Windows for now. `run_confined` returns
  `Unsupported` for `UnixSocketAccess::HostServices`.
- **Scope:** journal records and trusted CLI grant and revoke commands, not
  only platform primitives.

Facts observed on Windows 11 (build 26300) during the work:

- `CreateProcessW` with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` fails
  with `ERROR_FILE_NOT_FOUND` unless the AppContainer profile exists. A derived
  SID alone is not enough.
- With an explicit environment block that lacks `LOCALAPPDATA`, it fails with
  `ERROR_ENVVAR_NOT_FOUND`. Windows replaces `LOCALAPPDATA` and sets `TEMP` and
  `TMP` to the container's folder under `%LOCALAPPDATA%\Packages`.
- `CreateProcessW` maps the image with the caller's token, so a program the
  container cannot read still starts.
- `SetSecurityInfo` through a directory handle propagates an inheritable entry
  to existing children, does not follow a junction inside the tree, and
  recomputes inherited entries when the entry is removed.
- A hard link shares one security descriptor, so propagation reaches the file
  under its other names too.
- Loopback TCP from a container with no capabilities to a host listener is
  dropped without a reply; the connect times out with `WSAETIMEDOUT`.

## Decision

`omgcode_platform::process::run_confined` on Windows runs the tree in an
AppContainer, and a new `omgcode_platform::container` module manages the
container identity and its grants. The core journal records every grant. The
[execution guide](../execution.md#windows-confinement) owns the details.

- **Identity:** each workspace has one container,
  `OMGCode.Workspace.<workspace id as 32 hex digits>`. Its SID comes from
  `DeriveAppContainerSidFromAppContainerName`.
- **Profile:** the first grant creates the AppContainer profile with no
  capabilities. Revoking the container's last grant deletes the profile and
  its folder. `run_confined` refuses with a `Confinement` error when the
  profile does not exist; it never creates one.
- **Grants:** one explicit allow entry for the container SID, inherited by
  directories and files (`CONTAINER_INHERIT | OBJECT_INHERIT`):
  - read-execute: `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`;
  - read-write: also `FILE_GENERIC_WRITE | DELETE`, with no `WRITE_DAC`,
    `WRITE_OWNER`, or `FILE_DELETE_CHILD`.

  The directory is opened with backup semantics and without following a final
  reparse point, checked against its recorded volume and file identity,
  required to be NTFS, and changed with `SetEntriesInAclW` (`SET_ACCESS`) and
  `SetSecurityInfo` on that handle. The protected or unprotected DACL state is
  kept. The entry is read back from the same handle and must be exactly one
  allow entry with the expected mask and flags.
- **Revocation:** `REVOKE_ACCESS` for the SID, written the same way, then read
  back to confirm that no explicit allow entry remains.
- **Scope rules:** read-write grants only on the workspace directory itself.
  Read-execute grants on the workspace, a directory inside it, or a directory
  outside the development root. No grant may overlap application storage. One
  grant per workspace and directory. At most 32 grants per workspace and 256
  in all.
- **Journal:** `container_grant_requested` is synced before the ACL changes and
  advances the settings revision; it needs the expected revision.
  `container_grant_applied`, `container_revoke_requested`, and
  `container_grant_revoked` record outcomes and keep the revision. A grant
  whose outcome is unknown stays `pending` and can be revoked. A failed
  revocation stays `revoking` and can be retried without another request.
  Every append except those three outcomes must leave room, in events and
  bytes, for every outstanding grant to reach a confirmed revocation.
- **Runs:** `Confinement` gains `container: Option<ContainerName>`, which
  Windows requires and Linux ignores. Before anything starts, Windows checks:
  - `HostServices` is refused with `Unsupported`;
  - the profile exists;
  - `LOCALAPPDATA` is in the environment;
  - each `read_only` path allows the container at least read-execute, and
    each `read_write` path read-write, through entries for its SID or for
    every application package, less deny entries;
  - the program allows the container read-execute.

  The process attribute list then carries `SECURITY_CAPABILITIES` with the
  SID and no capabilities, next to the existing job and handle lists. The
  outcome reports `AppliedConfinement::AppContainer`.
- **CLI:** `omgcode workspace container-grant <id> <path> --access
  read-execute|read-write --revision <n>` and
  `omgcode workspace container-revoke <grant-id>`. `workspace status` lists
  outstanding grants and their states. `capabilities` reports
  `workspace_container_grants`. Nothing runs in the container from the CLI
  yet.

## Alternatives considered

- **Per-run grants removed after each run:** no lasting ACL changes, but every
  run rewrites and propagates the workspace DACL twice, which is slow on large
  trees. A crash would leave entries with no record of them. Rejected by the
  user in favour of recorded persistent grants.
- **A profile-less derived SID:** simpler and leaves no per-user state, but
  Windows refuses to start the process. Rejected after observation.
- **Creating the profile at run time:** would let a run recreate state that a
  revocation removed, outside the journal. Rejected; only a grant creates it.
- **Less privileged AppContainers (LPAC)**, which also drop the
  every-application-package SID: would block reads of system directories,
  which ordinary toolchains need, unless each one is granted. Deferred as a
  later hardening step.
- **Host services on Windows:** named pipes and Unix sockets are reachable only
  when their own ACL admits the container; widening that needs a reviewed
  design. Refused for now, as the user chose.
- **Walking the tree and setting entries ourselves:** would allow refusing
  hard-linked files, but duplicates the system's inheritance rules and breaks
  common hard-link layouts such as Cargo's `target` and pnpm stores. Rejected;
  the hard-link limit is documented instead.
- **Starting the program regardless of its ACL:** Windows would allow it,
  because the image is mapped with the caller's token. Rejected for parity with
  Linux, where an unlisted program cannot execute.

## Consequences

- Windows confined execution needs NTFS for granted directories, an existing
  container profile, and `LOCALAPPDATA` in the run environment.
- Grants are lasting ACL entries. Programs running as the same user can change
  them; the journal records only changes made through OMGCode. Grants that a
  run's `Confinement` does not list still apply to it.
- Paths that every application package may read, such as the Windows directory
  and `Program Files`, stay readable inside the container. The container can
  also write its own profile folder, where `TEMP` and `TMP` point.
- A grant reaches a hard-linked file under every name, including names outside
  the granted directory. Same-user actors who can create hard links are not
  isolated.
- Network access is denied, including loopback, but a loopback connect fails
  only after the system timeout instead of at once.
- Tests ran on Windows 11 build 26300 only. Nothing calls `run_confined` yet;
  C4 adds the CLI caller. This is not a qualification on other Windows builds.

## References

- [Execution guide](../execution.md)
- [ADR-0027](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md)
- [ADR-0028](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md)
- [AppContainer isolation](https://learn.microsoft.com/windows/win32/secauthz/appcontainer-isolation)
- [CreateAppContainerProfile](https://learn.microsoft.com/windows/win32/api/userenv/nf-userenv-createappcontainerprofile)
