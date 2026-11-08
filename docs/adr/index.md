# Architecture decision register

Record decisions whose rationale could otherwise be lost: a constraint,
a rejected plausible alternative, or a convention future code must follow.

Use the [template](template.md). Each record must include status and date.
Choose the next number above every existing record; never reuse a number,
renumber a record, or delete one to conceal a changed decision. Add each record
to this register in the same change.

Mark a changed decision superseded and link the replacing record. Update its
owning guide in the same change. A short-lived implementation detail with no
meaningful alternative does not need an ADR.

| Record | Status | Date |
| --- | --- | --- |
| [0000: Record architecture decisions](0000-record-architecture-decisions.md) | Accepted | 2026-11-08 |
| [0001: Adapt contributor rules to Rust](0001-adapt-contributor-rules-to-rust.md) | Accepted | 2026-11-08 |
| [0002: Require subscription authentication](0002-require-subscription-authentication.md) | Accepted | 2026-11-08 |
| [0003: Use scoped development toolchain profiles](0003-use-scoped-development-toolchain-profiles.md) | Accepted | 2026-11-08 |
