# Repository checks and branch protection

## Main branch

The active repository ruleset requires every update to `main` to arrive through
a pull request. It also blocks force-pushes and branch deletion.

- Ruleset: `main: pull requests only` (ID `26780301`).
- Target: `refs/heads/main`.
- Bypass actors: none, including no administrator bypass entry.
- Required approving reviews: zero.
- Required named status checks: none in this initial PR-only rule.

A pull request is required even for maintainers and administrators while the
rule is active. Repository administrators can still edit repository rules;
this setting does not remove their administrative authority.

Reviews and analysis results are visible on pull requests. Adding mandatory
approvals or named checks is a separate policy change. Do not add an unverified
or unavailable external check that would block every merge.

Use feature branches and merge through GitHub. Do not temporarily disable this
rule to publish workflow or documentation changes.

## CodeRabbit

The organization installation already covers this repository. The root
`.coderabbit.yaml` enables automatic and incremental reviews without the
default pause after a fixed number of reviewed commits. Its guideline list
includes contributor, coding, security, and testing rules.
The guides stored under docs are explicitly mapped to all repository paths;
their directory location must not limit application-code reviews.

A valid file or installed app is not proof of a review. Verify a real review on
the current pull-request head. CodeRabbit feedback needs human or coordinator
judgment; its presence does not establish that a change is correct.

## CodeQL

`.github/workflows/codeql.yml` runs for pull requests to main, pushes to main,
manual dispatch, and a weekly schedule.

The workflow always analyzes GitHub Actions. It adds Rust when tracked Rust
source and a Cargo manifest or rust-project.json exist, and JavaScript/TypeScript
when matching tracked source files exist. Rust source without an analysis
manifest fails language selection rather than being silently omitted.

Manifest discovery is repository-wide, not proof that every Rust file belongs
to a Cargo target. Inspect extraction diagnostics when source is introduced.
A successful selection or scan does not prove a full Cargo build or complete
semantic coverage of snippets outside the application project.

JavaScript detection excludes the extractor's default node_modules and
bower_components directories. The supported standalone extensions are .js,
.jsx, .ts, .tsx, .mjs, .cjs, .mts, and .cts. HTML-only inline scripts do not
activate this language in the current detector. Revisit that scope if the
frontend uses inline JavaScript; do not infer coverage from a green Actions scan.

Each selected language uses the no-build mode and security-extended queries.
No application build is claimed. CodeQL's Rust processing can still execute
build scripts or procedural macros; the analysis job has no Sonar or provider
secrets. Keep its permissions restricted.

The repository now contains Rust source in the core, CLI, and platform crates.
CodeQL selects Rust when tracked Rust files and a Cargo manifest are present.
Its no-build scan is separate from the locked Cargo checks in
`.github/workflows/rust.yml`; it is not a full application build or proof of
application security. There is no frontend source to scan yet. Add other source
languages deliberately when the implementation needs them.

On Ubuntu, the Rust workflow enables lingering for the runner user and waits
for its user service manager before the tests. The run and tree-limit tests
start delegated `systemd-run --user` scopes and fail without one.

## SonarQube Cloud

Project: [OMGCode](https://sonarcloud.io/project/overview?id=OMG-Software_OMGCode)
in the `omg-software` organization.

The analysis project key is `OMG-Software_OMGCode`. A Sonar administrator
changed it from `OMG-Software_OrangeCode` on 2026-11-11. The key identifies the
analysis history; it is not an application namespace. A GitHub repository
rename does not migrate a Sonar project key, so change the key on the server
before changing `sonar-project.properties`. The workflow requires a completed
analysis on the current pull-request head.

`.github/workflows/sonar.yml` uses CI-based analysis and
`sonar-project.properties`. Keep Sonar Automatic Analysis disabled to avoid
conflicting analysis methods.

Configure the `SONAR_TOKEN` repository secret with the intended project's
analysis credential. Never commit or print the token. The job fails explicitly
if the credential is missing. It waits for the server's quality-gate result.

To set up a personal analysis token:

1. Sign in to SonarQube Cloud using the GitHub account with analysis permission.
2. Open My account > Access Tokens > Personal Tokens.
3. Generate a token named omgcode-github-actions with an explicit expiry.
4. Copy it directly to GitHub Settings > Secrets and variables > Actions >
   New repository secret, with the name SONAR_TOKEN.

The token is created in Sonar, not GitHub. Do not paste it into a chat or commit
it. If the organization plan provides scoped organization tokens, prefer a
token restricted to this project's analysis and required read access.

Dependabot-triggered workflows use Dependabot secrets, not Actions secrets.
If Dependabot is enabled, configure SONAR_TOKEN in GitHub's Dependabot secret
store too. Without it, the credential check intentionally fails for those runs.

Fork pull requests do not receive the configured Sonar secret and skip this job.
Their other actions can still access a limited GITHUB_TOKEN. Do not use
`pull_request_target` to run fork code with the Sonar credential.
Use a trusted, separately reviewed path when a fork requires a Sonar analysis.

The source root is the repository, including Rust crates, workflow, and tooling
files. The explicit exclusions cover .git, target, node_modules, dist, and
coverage directories at any depth. They do not cover every generated directory;
review exclusions as the repository grows. Use the Rust workflow for compilation
and Clippy checks. Coverage reporting is not configured in either workflow.

Check the current Sonar result and quality gate for each pull request. Do not
present a missing, empty, or failed analysis as proof that application code
passed.

## Maintenance and verification

- Third-party actions use verified full commit pins with release comments.
  Review vendor releases before updating them.
- Check YAML, the CodeRabbit schema, workflow syntax, and changed documentation.
  Verify CodeQL language selection for absent and newly introduced source.
- After publishing through a pull request, inspect the actual review and
  completed workflow results. Check the head commit and final exit statuses.
- Confirm the effective main rules through the GitHub API. Do not test direct
  push rejection by attempting to land an unreviewed commit on main.
- Keep the [testing standards](testing-standards.md) and this guide accurate as
  application checks change. These integrations do not replace runtime,
  sandbox, credential, or platform verification.

## References

- [CodeRabbit configuration](https://docs.coderabbit.ai/reference/configuration)
- [CodeQL workflow options](https://docs.github.com/en/code-security/reference/code-scanning/workflow-configuration-options)
- [Sonar GitHub setup](https://docs.sonarsource.com/sonarqube-cloud/getting-started/github)
- [Sonar analysis methods](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/automatic-analysis)
- [Sonar personal tokens](https://docs.sonarsource.com/sonarqube-cloud/managing-your-account/managing-tokens)
- [Dependabot workflow credentials](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-on-actions)
- [Branch rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
