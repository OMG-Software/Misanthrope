# Provider authentication

## Required behaviour

The first release must support subscription-backed sign-in for both ChatGPT and
Claude, as well as separate API-key connections. An API-key-only implementation
does not meet this requirement.

Subscription support is a product requirement, not a claim of implementation or
provider approval. The application does not exist yet. Any unresolved provider
approval or technical restriction remains a release blocker for that feature;
do not silently replace it with API billing or defer it.

| Connection profile | Required authentication and usage path | Current design readiness |
| --- | --- | --- |
| OpenAI API | User API key; API-account billing | Existing API plan |
| ChatGPT subscription | Provider-supported browser sign-in and permission to use the user's plan | Official local/open-source integration documented; implementation and entitlement tests pending |
| Anthropic API | User API key; API-account billing | Existing API plan |
| Claude subscription | Provider-approved subscription sign-in and usage | Required; approval and integration route unresolved |
| Ollama Local | Local service configuration; authentication where configured | Existing local-provider plan |
| Ollama Cloud | Provider cloud credentials | Existing cloud-provider plan |
| OpenRouter | User API key and explicit upstream routing | Existing routing plan |

The provider, authentication method, account/workspace, billing mode, and model
capabilities form one connection profile. Do not infer all of these from a
provider name.

## Current provider evidence

Checked against official documentation on 2026-11-08. Recheck before
implementation and release; provider availability and requirements can change.

### ChatGPT

OpenAI documents Sign in with ChatGPT plan usage for local and open-source apps.
Identity sign-in and permission to consume plan usage are separate.
[OpenAI integration overview](https://developers.openai.com/siwc/token-sharing-open-source)

Use the documented browser authorization flow with PKCE, state, nonce,
loopback-callback validation, and validation of the returned identity and scopes.
Persist the issued account/workspace registration and installation host identity
as specified by OpenAI. Do not borrow another application's OAuth client
identity or scrape an existing login cache.
[Registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)

The currently documented plan route has a restricted Responses API surface,
including streamed requests with storage disabled and limits on request fields,
conversation state, and hosted tools. Treat this as a distinct adapter capability
profile from API-key access.
[Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)

A completed sign-in is not evidence that the selected model request can consume
subscription usage. Verify granted plan permission and a permitted completed
request. Recheck eligibility and limits rather than promising every ChatGPT
plan or model. Review integration eligibility again if distribution changes
from the documented local/open-source model.

### Claude

Anthropic documents subscription login for its own Claude Code product.
That does not by itself establish permission for Misanthrope to expose the same
login or usage allowance.
[Claude Code authentication](https://code.claude.com/docs/en/authentication)

The Agent SDK documentation currently requires prior Anthropic approval for a
third-party product to offer claude.ai login or subscription usage limits.
No such approval has been established for Misanthrope.
[Agent SDK integration restriction](https://code.claude.com/docs/en/agent-sdk/overview)

Obtain approval or an explicit supported integration route covering Misanthrope
before calling this feature available. A CLI or SDK wrapper alone is not
evidence of that approval. Keep the requirement open and the blocker visible;
a separately billed Anthropic API connection remains a different profile.

## Settings and account lifecycle

- Provide separate subscription and API-key connection choices for OpenAI and
  Anthropic/Claude. Identify the account, workspace where applicable, selected
  model, and billing mode before a session sends project data.
- Show sign-in, reconnect, and sign-out actions. Distinguish connected identity,
  permission to use the plan, model access, expired credentials, quota reached,
  provider restriction, and unavailable integration.
- Open provider login in the system browser. The application must not collect
  the user's ChatGPT or Claude password.
- Let the user cancel login. Reject mismatched, expired, duplicated, or
  unsolicited callbacks. A new attempt must not replace an active connection
  until identity and account binding are validated.
- Refresh credentials through the supported provider flow. Serialize refreshes
  for one credential set and store rotated credentials safely.
- Support account changes and reconnect after revocation. Clear local credentials
  on sign-out and perform remote revocation when the provider supports it.
  Do not imply that local sign-out necessarily revokes every remote token.
- A subscription quota or authentication failure must not trigger paid API
  fallback without an explicit user-selected policy.
- Show available usage/limit information, or state when it is unavailable.
  Do not invent monetary costs or remaining quota from a subscription name.

## Credential and execution boundaries

Keep access tokens, refresh tokens, API keys, login codes, and verifier values
in the trusted authentication service. Store durable credentials in protected
OS storage. Do not silently fall back to plaintext when that store is
unavailable; offer an explicit memory-only session if appropriate.

Expose opaque connection identifiers and safe account labels to the UI. Never
put credentials in model context, workspace files, terminal environments,
command arguments, transcripts, or diagnostics. Scope credentials to the
correct provider, account/workspace, and audience.

The direct model adapter does not gain filesystem or command permissions.
A future approved provider-runtime integration needs its own reviewed
credential and execution boundary. Its built-in tools must remain constrained
by Misanthrope's effective policy and OS containment.

## Acceptance evidence

Before claiming subscription support for either provider, verify on Windows
and Linux:

1. A documented or approved integration route applies to Misanthrope.
2. A fresh account connection completes through the supported user flow.
3. Plan consent and model access are validated independently from identity.
4. A bounded, user-authorized request completes using the subscription path.
5. Refresh, expiry, revocation, sign-out, and account switching behave correctly.
6. Refused consent, ineligible plans, quota limits, and network failure produce
   clear results without unexpected API charges.
7. Project tasks and untrusted UI content cannot obtain tokens, alter account
   binding, or expand execution permissions.

Use deterministic mocks for routine tests. Live subscription checks require an
eligible account, explicit opt-in, minimal test data, and recorded results.
No such runtime checks have run yet.

See the [security rules](security-coding-rules.md),
[testing standards](testing-standards.md), and
[subscription decision](adr/0002-require-subscription-authentication.md).
