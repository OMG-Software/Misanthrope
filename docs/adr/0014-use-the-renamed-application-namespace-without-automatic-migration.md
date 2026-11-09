# 0014: Use the renamed application namespace without automatic migration

- Status: Accepted
- Date: 2026-11-09

## Context

The user renamed the project to OrangeCode and requires the new name throughout
the repository. The name also selects commands, token variables, default private
storage, and the validated session filename prefix. Discovering or moving saved
state during a branding change could select unintended data or invalidate native
directory registrations.

## Decision

Use OrangeCode for display and repository identity, orangecode for Rust crates,
the command, and session prefixes, and ORANGECODE for environment-variable names.
Advance the compiled prompt version when its identity text changes.

Do not retain old-name aliases or automatically discover, import, or move private
data. Preserve the JSONL schemas and fail-closed directory-identity checks.
An operator can explicitly select the original private directory and explicitly
rename a session filename prefix after closing writers and making a backup.
The [development guide](../development.md) owns these transition instructions.

## Alternatives considered

- Keep old command, environment, and session-prefix aliases: rejected because
  the requested rename covers all repository occurrences and this is an early
  prototype without a stable release compatibility contract.
- Automatically discover or move earlier private storage: rejected because
  branding is not authority to import state, and relocation can invalidate
  recorded path and native-directory identity.
- Rewrite journal content to reflect the new brand: rejected because a filename
  transition does not require changing historical events or granting authority.

## Consequences

New runs use one consistent namespace. Existing private data remains in place.
Operators must select earlier settings explicitly and change an earlier session
filename prefix before reopening it. No journal is replayed as work. A future
release migration must have its own explicit data-integrity and recovery design.

## References

- [Development and data transition](../development.md)
- [Session storage](../session-storage.md)
- [Workspace permissions](../workspace-permissions.md)
- [Compiled instruction contract](../agent-instructions.md)
