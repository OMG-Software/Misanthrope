# 0008: Compile the base prompt and separate user preferences

- Status: Accepted
- Date: 2026-11-08

## Context

The user wants the system prompt hardcoded because it carries important security
guidance. Users also need saved customization and an ordinary current request.
Showing these together does not require giving them equal instruction priority.

## Decision

Compile the versioned base prompt into the Rust application and display its
actual bytes read-only. Keep its editable source in the repository for review.
Change the installed base through an application build/update, not a runtime
override from configuration, files, environment, or journal history.

Keep saved preferences and the current request as separate user-level input.
The explicit task can override saved preferences within the base contract.
Adapters must preserve this distinction using the selected route's supported
fields, rather than treating every source as a new privileged system prompt.
See the [instruction contract](../agent-instructions.md).

## Alternatives considered

- A replaceable prompt file is convenient but lets runtime configuration replace
  application security guidance. Use a compiled resource with visible source.
- Appending user text in the base instruction channel gives customization the
  same message authority. Supply it as user-level preferences instead.
- Hiding the base prevents inspection and does not make text an enforcement
  boundary. Keep it visible and enforce permissions independently.
- Assuming a fixed number of API messages breaks provider/context portability.
  Define logical sources and test each serialization contract.

## Consequences

Prompt updates require a new build, and source text/version/digest must agree
with the compiled resource. Request manifests and caches include the new prompt
revision. A modified executable or user fork remains under the user's control.
Hardcoding does not guarantee that a model obeys the prompt or resists every
injection. Runtime authorization and OS containment remain required.

## References

- [Context policy decision](0005-keep-agent-context-subordinate-to-runtime-policy.md)
- [Security coding rules](../security-coding-rules.md)
- [System prompt source](../../prompts/system.md)
