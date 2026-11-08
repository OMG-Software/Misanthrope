# Skills and settings

This document defines the planned skills manager. No skill loader or settings
page is implemented yet. Skills are reusable task instructions and resources.
They are not permission grants or trusted application extensions.

## Settings page and scope

Provide Add, Remove, Enable, and Disable controls. Each entry shows its name,
description, source, pinned revision/version, content digest, compatibility,
requested tools, enabled scope, and validation status. Show its instructions
and resource list before enablement. Render all imported text safely.

Store skill packages in application-managed, per-user storage outside the
task-writable workspace. Maintain a separate core-owned registry of enablement.
Support application defaults, workspace overrides, and session overrides, with
the narrower scope taking precedence. Show the effective set and any override.
A skill disabled in the effective set must not be selected or loaded.

Adding imports a validated immutable copy and leaves it disabled until the user
enables it. Initial sources are a selected local folder or an explicitly selected
Git source pinned to an immutable commit. Fetching a remote source requires its
own permitted network operation. Neither import nor update runs scripts,
installs dependencies, enables the package, or follows a mutable latest version.
Updates require review of source/content changes and explicit user enablement
of the new version. Keep the old pinned version until that replacement is chosen.

Remove disables the registration and deletes its managed package when no active
reader needs it. It must not delete the user's original folder or Git repository.
Keep minimal audit identifiers under the history policy, not an executable copy
of a removed package. Do not promise to erase content already sent to a provider.

## Package format and validation

Adopt the [Agent Skills format](https://agentskills.io/specification) for the
initial package profile: a directory containing SKILL.md with YAML frontmatter
and a Markdown body. Validate the specification's required name and description,
field types/limits, naming rules, and directory-name match. Scripts, references,
and assets are optional. Preserve license and compatibility metadata.

Misanthrope may reject unsafe or unsupported packages even when their metadata
is format-valid. Bound file count, total bytes, individual file sizes, parser
nesting, reference depth, and resource loading. Reject duplicate or ambiguous
metadata keys and unsafe YAML types. Do not fetch remote references implicitly.

For imports and resource reads, reject traversal, absolute paths, escaping
symlinks/junctions, unsafe hard links, and path-swap races. Copy authorized file
content into owned storage rather than retaining writable links to the source.
Apply the same rules to Git metadata, checkout behaviour, and any future archive
support. Validate content before publishing a registry entry. An interrupted
import must not leave a partially enabled package.

The format's experimental allowed-tools field is advisory metadata. It may
narrow tool selection within current policy. It cannot pre-approve execution,
change a permission switch, install a tool, or grant network or filesystem access.
Unsupported requirements must produce a visible compatibility result.

## Loading and execution

Load only minimal catalog metadata for effectively enabled skills. Load a
relevant skill body on explicit user selection or agent selection from that
enabled set. Load bounded references and assets only when needed. Record which
content was actually selected in the request manifest and show it to the user.
Repository content cannot register or enable a skill automatically.

Keep skill text source-labelled and below the base prompt and explicit user task.
Do not promote it into trusted custom instructions. If it asks to ignore policy,
reveal credentials, expand scope, or run a hidden command, treat that request as
untrusted content. Enabling a skill is consent to make its content available;
it is not approval of every action the content proposes.

Scripts and helpers use the ordinary [controlled runner](development-tools.md),
with the effective toolchain profile and current operation permissions. Apply
the same containment, limits, descendant cleanup, and output handling as any
other command. Package downloads and installations need their own authorization.
Do not use an import hook, host plugin API, or unsupervised interpreter path.

## Disable, removal, and active sessions

Apply enablement changes before the next request. A request already in flight
must be visibly associated with its old skill manifest. Disabling or removing a
skill blocks new loads and dependent tool dispatch. Stop and observe completion
of active tasks attributed to that skill before reporting the change complete.
Bind model-generated tool requests and their child tasks to the request's skill
manifest. Conservatively treat them as dependent on every selected skill when
finer attribution cannot be established. Do not trust model-declared attribution.

Invalidate derived context and cache entries that depend on changed or removed
skill content. Rebuild the next context without disabled skill bodies/resources
or summaries that carry their instructions. Preserve factual task outcomes as
labelled history where appropriate, using original evidence instead of blindly
retaining an old summary. Check old model responses against the current registry
before dispatch. Already-seen text cannot be made unseen; core authorization
must remain effective regardless of what the model remembers.

## Acceptance requirements

Test valid and malformed packages; duplicate metadata; name collisions; source
updates; large/deep resources; link/traversal/race attacks; interrupted import;
safe rendering; effective-scope overrides; disabled selection; lazy loading;
allowed-tools escalation attempts; script confinement; disable during streaming
and execution; cache/summary invalidation; and removal without deleting originals.
Document the supported format subset and verify it on Windows and Linux.
