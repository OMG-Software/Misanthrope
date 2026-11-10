# Bundled skill source packages

This directory contains reviewable source packages for the planned OMGCode
skills manager. It is a catalog of source files, not an active skill registry.
OMGCode does not yet load, register, enable, or execute these packages.

| Package | Purpose | Files |
| --- | --- | --- |
| [pull-request-review](pull-request-review/SKILL.md) | Evidence-based review of pull requests, branches, and local changes | SKILL.md only |

## Provenance and integrity

The user supplied the local pull-request-review package. Its SKILL.md was copied
without changing its frontmatter, instructions, examples, or line endings.

- Encoding: UTF-8 with LF line endings.
- Size: 23,504 bytes.
- SHA-256: `c1afecb30ead5c744ea24639dcd9f3e253dd0791216b2dc635ae7c9444289840`.
- No scripts, resources, assets, or dependencies accompany this snapshot.

The source does not declare license or compatibility metadata; those fields
have not been invented or added to the imported file.

## Availability and trust

The package describes review, fix, and posting workflows. Its references to
GitHub CLI commands, independent agents, and a built-in `/code-review` describe
requirements for a compatible future runtime. They do not establish that
OMGCode implements those features.

Adding this source does not enable the skill or authorize a review, a code
change, a command, or an external post. Future import must validate and copy it
into application-managed storage outside the task workspace, initially disabled.
Enablement must remain a separate trusted user action. Follow the owning
[skills contract](../docs/skills.md) for loading, permissions, and lifecycle.
