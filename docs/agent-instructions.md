# Agent instructions and prompt visibility

This document defines planned behaviour. No prompt assembly or settings page is
implemented yet. Use it with the [security rules](security-coding-rules.md).

## Product priorities

Misanthrope has three goals: secure operation, defence against prompt injection,
and efficient use of models and development resources. Security takes priority
when an optimization would weaken containment, authorization, or confidentiality.
Prompt injection defence requires runtime controls as well as model guidance.
No prompt can guarantee that a model will ignore malicious instructions.

## Visible system prompt

The SYSTEM prompt settings page must show:

- The complete bundled base prompt, its version, and its content digest.
- Editable user additions at application, workspace, and session scope.
- The effective order, source, and revision of those additions.
- A preview of Misanthrope's assembled instructions for the next request.
- The selected provider profile and the token count or labelled estimate.

The initial base text is [prompt version 0.1.0](../prompts/system.md). It is a
reviewable draft. Store it as a versioned application resource. Show the base
text as read-only and place user additions in separate editable fields. Do not
hide application-owned instruction fragments from the user.

The preview covers instructions Misanthrope controls. It cannot reveal a
provider's private internal instructions. Exclude credentials from all prompt
content. Show a separate request inspector for selected skill content, source
references, tool definitions, and context omissions, with safe rendering.

## Assembly and precedence

Use a stable sequence: base prompt, application additions, workspace additions,
then session additions. Label every block. A narrower user addition takes
precedence over a conflicting broader addition. The current explicit user task
can refine these preferences. User additions cannot override runtime policy or
the base requirements for truthful reporting and handling untrusted content.

Do not copy repository files, retrieved text, tool output, or skill bodies into
the trusted user-addition fields. Supply these as source-labelled task context
through the adapter's supported representation. Project conventions may guide
implementation within the user's task; they cannot grant authority or expand it.

SYSTEM is a product concept, not a required literal wire role. Each provider
adapter must map these instructions to its supported instruction fields. Test
the mapping for the exact endpoint and authentication mode. If a route cannot
preserve the required contract, report that limitation instead of silently
dropping instructions or sending a rejected role.

The Rust core owns the effective permission policy. Displaying a policy snapshot
to the model is explanatory; that text is never the authorization source.
All tool calls still require current core authorization and OS containment.

## Changes and request records

Freeze an instruction manifest when starting each request. Record the prompt
version/digest, custom-instruction revisions, selected skill versions, tool
schema revision, provider/account profile ID, and source references. Record safe
identifiers, never credential values. Keep previous manifests and retained
instruction snapshots with the session journal under its retention policy so
the user can inspect what was sent. A digest alone cannot reconstruct old text.
Show when retention or removal has made an older body unavailable.

Edits apply to the next request. Clearly label a request already in flight. If an
edit requires immediate effect, cancel and observe completion before restarting;
do not silently treat an old response as produced under new instructions.
Invalidate derived context and cache entries whose instruction inputs changed.
Keep the base prompt and current user additions outside generated summaries.

## Acceptance requirements

- Verify that the preview matches the application-owned instructions actually
  serialized by each provider adapter, including subscription profiles.
- Verify ordering, conflicting scopes, Unicode, empty additions, size limits,
  and revision changes during streaming and compaction.
- Put malicious instructions in files, tool output, skills, summaries, and
  rendered content. Verify that they cannot change permissions or trusted fields.
- Verify safe rendering and absence of credentials in previews and journals.
- Evaluate the draft prompt against useful coding tasks and adversarial cases.
  Measure unauthorized actions at the enforcement boundary, not model promises.

See [context management](context-management.md) and [skills](skills.md) for their
additional lifecycle and verification requirements.
