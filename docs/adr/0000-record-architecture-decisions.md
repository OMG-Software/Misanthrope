# 0000: Record architecture decisions

- Status: Accepted
- Date: 2026-11-08

## Context

Misanthrope is starting with design and contribution rules before implementation.
Future contributors need the reasons behind lasting constraints. Discussion and
commit history alone are difficult to use as an active design reference.

## Decision

Keep short, numbered Markdown records in this directory. Follow the
[register and numbering rules](index.md) and use the [template](template.md).
Keep normative rules in their owning guide; records explain their rationale.
Record only choices with a plausible alternative or a lasting constraint.

## Alternatives considered

- Rely only on chat or pull-request discussion: the rationale is separated from
  the current project documentation.
- Put all decisions in one growing document: individual status and supersession
  become harder to track.
- Record every implementation detail: the register would obscure important
  constraints with routine facts.

## Consequences

Design choices have a stable reference and an explicit status. Contributors
must maintain the index and owning guide when a decision changes.
