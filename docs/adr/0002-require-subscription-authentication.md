# 0002: Require subscription authentication

- Status: Accepted
- Date: 2026-11-08

## Context

The project owner requires both ChatGPT and Claude subscription authentication.
The original README covered API connections and left subscription integration
for a later design. Subscription identity, usage entitlement, and API billing
are different contracts.

## Decision

Make both subscription paths first-release requirements alongside API-key
connections. The [authentication guide](../provider-authentication.md) owns
the connection profiles, current provider evidence, and acceptance gates.

Use a provider-supported integration for each path. Prefer the documented
direct ChatGPT plan flow for the local open-source application. Keep the Claude
route visibly unresolved until approval or an explicit supported route covers
Misanthrope. This is a readiness gate, not removal of the requirement.

Keep billing mode explicit and prevent unapproved fallback to API charges.
Do not import another application's credentials or assume a CLI wrapper
establishes third-party subscription permission.

## Alternatives considered

- Ship API-key access only: this does not meet the requested subscription scope.
- Treat subscription credentials as ordinary API keys: this loses account,
  entitlement, consent, capability, and billing distinctions.
- Treat another tool's login cache or CLI wrapper as approval: this provides no
  evidence that Misanthrope is covered and obscures the credential boundary.
- Drop Claude subscription support because its route is unresolved: this would
  change the owner's requirement without agreement.

## Consequences

Connection identity and capability data must include authentication and billing
mode. Integration feasibility is an early milestone. No authentication code,
live entitlement test, or Anthropic approval exists yet.

## References

- [Authentication requirements and official sources](../provider-authentication.md)
