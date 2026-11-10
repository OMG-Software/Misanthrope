# 0020: Rename the project to OMGCode

- Status: Accepted
- Date: 2026-11-10
- Supersedes: [ADR-0014](0014-use-the-renamed-application-namespace-without-automatic-migration.md)

## Context

The user requested OMGCode as the project and repository name. The application
name also selects the command, Rust crates, token variable, default private
storage, and accepted session filename prefix. A rename must not import private
data or change the meaning of stored authority records.

## Decision

Use OMGCode for the project, GitHub repository, and application-data identity.
Use `omgcode` for the command and session prefix, `omgcode-*` for Rust packages,
and `OMGCODE_LM_TOKEN` for the local-model token. Set the application version to
0.7.0 because these public names change. Advance the compiled prompt to 0.1.4
with the same instruction structure and the new identity.

Retain ADR-0014's data-integrity rules: no old-name runtime aliases, automatic
discovery, import, relocation, or journal rewriting. Keep JSONL schemas and
native directory checks unchanged. An operator can explicitly select an earlier
data directory and change only a closed session's filename prefix, after making
a private backup. The [development guide](../development.md) owns this procedure.

Keep the existing Sonar analysis key until a separate server-side migration is
verified. Change its configured display name. The repository rename does not
authorize a new analysis project, token, or permission change.

## Alternatives considered

- Keep earlier commands or token aliases: rejected because the user requested
  one project identity and this prototype has no stable release contract.
- Automatically move private storage: rejected because directory identity and
  stored workspace registrations can become invalid after relocation.
- Change the Sonar project key with a text replacement: rejected because that
  can disconnect the existing project's history and quality-gate configuration.
- Include unfinished checkpoint-storage work in the rename: rejected because
  it has a separate scope and must remain available in the original checkout.

## Consequences

Scripts and downstream Rust dependencies must use the new names. Existing
private data stays in place. Operators must select earlier settings explicitly
and change the session filename prefix before reopening an earlier session.
Historical journal bytes and permissions do not change. Old identity text stays
only where it explains the transition, rejected input, or the external Sonar key.

## References

- [Development and data transition](../development.md)
- [Session storage](../session-storage.md)
- [Compiled instruction contract](../agent-instructions.md)
- [Repository analysis](../repository-checks.md)
