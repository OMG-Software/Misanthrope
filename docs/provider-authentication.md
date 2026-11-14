# Provider authentication

## Release requirements

The first release must support subscription-backed sign-in for both ChatGPT and
Claude, as well as separate API-key connections. An API-key-only implementation
does not meet this requirement.

Subscription support is a product requirement, not a claim of implementation or
provider approval. The current prototype has no sign-in, account management, or
subscription adapter. Any unresolved provider approval or technical restriction
remains a release blocker for that feature; do not silently replace it with API
billing or defer it.

| Connection profile | Required authentication and usage path | Current design readiness |
| --- | --- | --- |
| OpenAI API | User API key; API-account billing | Planned; not implemented |
| ChatGPT subscription | Provider-supported browser sign-in and permission to use the user's plan | Official local/open-source integration documented; implementation and entitlement tests pending |
| Anthropic API | User API key; API-account billing | Planned; not implemented |
| Claude subscription | Provider-approved subscription sign-in and usage | Required; approval and integration route unresolved |
| Ollama Local | Local service configuration; authentication where configured | Planned; not implemented |
| Ollama Cloud | Provider cloud credentials | Planned; not implemented |
| OpenRouter | User API key and explicit upstream routing | Planned; not implemented |
| LM Studio | Explicit local server profile; LM Studio API token when required | Local loopback chat prototype tested with Gemma on Windows; credential service and full platform/auth qualification pending |

The provider, authentication method, account/workspace, billing mode, and model
capabilities form one connection profile. Do not infer all of these from a
provider name.

## Implemented prototype (v0.1.0)

The current CLI has one non-streaming LM Studio Chat Completions adapter. The
caller supplies an already available model name, a maximum output budget, and a
context budget with `--context-tokens`. For example, the CLI test fixture passes
131072 as its configured context budget. This value is caller input; the
prototype does not discover or verify the model's actual capacity. The adapter
estimates input use from UTF-8 bytes, message count, fixed overhead, and output
budget. It does not use the model's tokenizer or perform compaction.

The adapter accepts only plain HTTP URLs with a literal loopback IP, an explicit
nonzero port, and a `/v1` path, for example `127.0.0.1` or `[::1]`. It rejects hostnames,
non-loopback addresses, credentials in the URL, query strings, and fragments.
The request uses `/v1/chat/completions`, disables proxy use and redirects, and
does not retry. It sends no tools and rejects tool-call responses. The adapter
does not discover models, start the LM Studio service, download models, or
configure that service.

The CLI accepts an optional `OMGCODE_LM_TOKEN` in its own process
environment. The core keeps the resulting authorization header in memory and
does not add the token to a request manifest. There is no OS credential service
or token settings UI. This is a prototype-only developer input. Task execution
and task environments do not exist in the prototype; this does not qualify the
future task-environment credential boundary.

The CLI redacts exact occurrences of that currently configured token from new
user text, preferences, titles, and assistant text before retention or display.
Both chat-created sessions and direct `session new` use the same title redaction.
It also redacts the current token from historical text before sending context.
In tool-enabled chat, a tool call whose identifier or arguments contain the
token is refused before it is recorded, because redacting its arguments could
change the requested path. This is known-token redaction, not general secret
detection or retroactive cleanup of existing journals. Ordinary string storage does not guarantee erasure.

The adapter returns assistant text, `finish_reason`, and optional provider usage
to the CLI, which emits them in JSON output. Tool-enabled chat emits the final
reply's `finish_reason` but no usage. The session journal stores the
assistant text but not provider usage or `finish_reason`. Loopback identifies
the network destination used by the adapter; it does not prove that the selected
LM Studio server or model avoids upstream/cloud services.

On 2026-11-08, a Windows smoke check used the user's loaded
`google/gemma-4-e4b` Q4_K_M model at `http://127.0.0.1:1234/v1`, without a token.
LM Studio reported a loaded context of 131,072 tokens. Two bounded CLI requests
completed: the second resumed the saved history and recalled a harmless test
word. Archive and restore preserved that journal. This checks that one live
profile; it does not qualify token authentication, streaming, Linux inference,
all models, or the full release contract. The adapter accepts empty/null action
fields as no action and still rejects actual tool requests.

These are prototype facts. The rest of this document defines the broader
authentication and provider requirements for release.

## Current provider evidence

Checked against official documentation on 2026-11-08. Recheck before release;
provider availability and requirements can change.

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
That does not by itself establish permission for OMGCode to expose the same
login or usage allowance.
[Claude Code authentication](https://code.claude.com/docs/en/authentication)

The Agent SDK documentation currently requires prior Anthropic approval for a
third-party product to offer claude.ai login or subscription usage limits.
No such approval has been established for OMGCode.
[Agent SDK integration restriction](https://code.claude.com/docs/en/agent-sdk/overview)

Obtain approval or an explicit supported integration route covering OMGCode
before calling this feature available. A CLI or SDK wrapper alone is not
evidence of that approval. Keep the requirement open and the blocker visible;
a separately billed Anthropic API connection remains a different profile.

## Recorded provider profiles (0.24.0)

`workspace provider-profile-add/show/remove` record a name, a provider kind,
and an authentication mode. Kinds are `openai`, `anthropic`, `ollama_local`,
`ollama_cloud`, `open_router`, and `lm_studio`; modes are `subscription`,
`api_key`, and `none`. Subscription is accepted only for OpenAI and Anthropic.
API keys are accepted for those, Ollama Cloud, OpenRouter, and LM Studio.
Ollama Local accepts only `none`.

A profile stores no endpoint, account, token, password, certificate, or other
credential. It is configuration only: no adapter, login, entitlement check,
billing mode, or model context uses it. `capabilities` reports
`provider_profiles: true` and `provider_adapters: false`. A credential service
and the provider-specific login approvals remain prerequisites for any real
connection. See [ADR-0038](adr/0038-record-provider-profiles-without-credentials.md).

## Settings and account lifecycle

### LM Studio release connection requirements

LM Studio documents OpenAI-compatible model, Chat Completions, and Responses
endpoints. It also supports API tokens, with server authentication disabled by
default in the current documentation. Use a distinct LM Studio connection
profile; do not send an OpenAI API key or ChatGPT subscription token to it.
[API compatibility](https://lmstudio.ai/docs/developer/openai-compat),
[server authentication](https://lmstudio.ai/docs/developer/core/authentication)

Provide an explicit server URL, model selection/discovery, endpoint mode,
connection test, and token configuration. Suggest loopback
http://localhost:1234/v1 as an editable starting address, not as proof of a running
server. Allow a no-token profile only for the explicitly selected local service.
If authentication is enabled, use its scoped LM Studio token through the
credential service. A separately selected non-loopback service requires
authenticated HTTPS; never silently redirect requests or credentials.

Qualify Chat Completions and Responses independently for the chosen server
version and model. Reuse wire-format code only where its contract matches.
Model IDs, chat templates, token accounting, context limits, streaming, structured
output, and tool-call reliability are not established by the OpenAI-compatible
label. Resolve the loaded model's safe context budget or require a validated
profile. Malformed tool-like text remains text; do not execute it heuristically.
[Tool use](https://lmstudio.ai/docs/developer/openai-compat/tools)

Keep local session history and portable compaction in OMGCode. Do not assume
that provider-native caching, state, or compaction has the OpenAI API contract.
Do not submit provider-managed Remote MCP or hosted execution tools through this
profile. Route MCP through the [core-owned clients](mcp.md); a provider's own tool
execution could bypass those controls.
[LM Studio Responses features](https://lmstudio.ai/docs/developer/openai-compat/responses)

Starting the service, changing its settings, downloading models, or installing
software are separate explicit operations. Connecting for inference does not
authorize them. If the selected service loads an already installed model on
demand, describe that as part of the selected inference profile and qualify its
resource limits. This grants no general model-management authority.

Test model discovery, absent/unloaded models, token and no-token modes,
authentication failure, streaming/cancellation, validated tool calls, changed
context limits, and absence of unexpected cloud fallback on both supported
platforms. The current prototype does not implement model discovery, streaming,
or tool calls. The Windows Gemma smoke check above covers one live profile;
full platform and authentication qualification remains pending.

### Shared settings and lifecycle

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

For release, keep access tokens, refresh tokens, API keys, login codes, and
verifier values in the trusted authentication service. Store durable credentials
in protected OS storage. Do not silently fall back to plaintext when that store
is unavailable; offer an explicit memory-only session if appropriate. The
prototype's `OMGCODE_LM_TOKEN` process-environment input does not meet this
credential-service requirement and must not be treated as its replacement.

Expose opaque connection identifiers and safe account labels to the UI. For
release, never put credentials in model context, workspace files, task or
terminal environments, command arguments, transcripts, or diagnostics. Scope
credentials to the correct provider, account/workspace, and audience. The
prototype has no task processes; its CLI reads the optional LM Studio token
from its own process environment and does not place that value in the journal.

The direct model adapter does not gain filesystem or command permissions.
A future approved provider-runtime integration needs its own reviewed
credential and execution boundary. Its built-in tools must remain constrained
by OMGCode's effective policy and OS containment.

## Acceptance evidence

Before claiming subscription support for either provider, verify on Windows
and Linux:

1. A documented or approved integration route applies to OMGCode.
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
