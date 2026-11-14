# 0038: Record provider profiles without credentials

- Status: Accepted
- Date: 2026-11-14

## Context

The project plans additional providers: OpenAI, Anthropic/Claude, Ollama Local,
Ollama Cloud, OpenRouter, and the existing local LM Studio route. Subscription
and API-key connections must stay separate, and the current CLI has no
operating-system credential service. Provider login and entitlement routes
still need provider-specific decisions.

A settings surface is needed before adapters, but it must not become a
credential store or a runtime authorization.

## Decision

Record operator-declared provider connection shapes:

```text
omgcode workspace provider-profile-add <name> --kind <kind> --auth <auth>
    --revision <n>
omgcode workspace provider-profile-show <name>
omgcode workspace provider-profile-remove <name> --revision <n>
```

- **Contents:** a profile name, a provider kind, and an authentication mode.
  It stores no endpoint, account, token, password, certificate, or other
  credential.
- **Kinds:** `openai`, `anthropic`, `ollama_local`, `ollama_cloud`,
  `open_router`, `lm_studio`.
- **Authentication:** `subscription`, `api_key`, or `none`. Subscription is
  accepted only for OpenAI and Anthropic. API keys are accepted for those,
  Ollama Cloud, OpenRouter, and LM Studio. Local providers use `none`;
  Ollama Local accepts only `none`.
- **Shape:** at most 32 profiles, names follow the existing profile-name rules
  and compare case-insensitively. Each change uses the expected settings
  revision and records `provider_profile_added` or `provider_profile_removed`.
- **Not runtime:** no adapter, login, entitlement check, billing mode, or
  model context uses a profile. `capabilities` reports
  `provider_profiles: true` and `provider_adapters: false`.

A later provider increment must add a reviewed credential service, keep
subscription and API-key usage separate, never silently fall back to paid API
usage, and bind credentials to the provider, account, and endpoint.

## Alternatives considered

- **Store tokens or API keys in the profile.** Rejected: the settings journal
  is not a credential store, and values are shown by `status`.
- **Store endpoints and accounts now.** Deferred: an endpoint is meaningful
  only with an adapter and a credential policy, and it would invite secrets in
  the profile.
- **Wait for adapters before adding profiles.** Rejected: the separation of
  subscription, API-key, and local modes is a contract worth fixing first.

## Consequences

- Operators can prepare connection shapes, but nothing connects or
  authenticates.
- A credential service remains a prerequisite for any real provider adapter.
- The control journal gains provider events. A journal written by 0.24.0
  cannot be read by 0.23.0.
- Provider profiles are configuration, not permission. A future adapter must
  still check the selected profile, account, entitlement, and billing mode at
  call time.

## References

- [Provider authentication](../provider-authentication.md)
- [Workspace settings](../workspace-permissions.md)
- [Security coding rules](../security-coding-rules.md#provider-traffic-and-tool-calls)
- [Testing standards](../testing-standards.md)
