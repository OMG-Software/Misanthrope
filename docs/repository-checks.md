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

Each selected language uses the no-build mode and security-extended queries.
No application build is claimed. CodeQL's Rust processing can still execute
build scripts or procedural macros; the analysis job has no Sonar or provider
secrets. Keep its permissions restricted.

The current documentation-only repository has no Rust or frontend code to scan.
A successful Actions analysis is not application vulnerability coverage.
Add other source languages deliberately when the implementation needs them.

## SonarQube Cloud

Project: [Misanthrope](https://sonarcloud.io/project/overview?id=OMG-Software_Misanthrope)
in the `omg-software` organization.

`.github/workflows/sonar.yml` uses CI-based analysis and
`sonar-project.properties`. Keep Sonar Automatic Analysis disabled to avoid
conflicting analysis methods.

Configure the `SONAR_TOKEN` repository secret with the intended project's
analysis credential. Never commit or print the token. The job fails explicitly
if the credential is missing. It waits for the server's quality-gate result.

Fork pull requests do not receive secrets and skip this job. Do not use
`pull_request_target` to run fork code with the Sonar credential.
Use a trusted, separately reviewed path when a fork requires a Sonar analysis.

The source root is the repository, including workflow and tooling files.
Generated build directories, dependency trees, and coverage output are excluded.
Update source/test classification and report paths with the application scaffold.
No Rust compilation, Clippy execution, or coverage import is configured here.

At setup, the Sonar project existed but had no completed analysis. A repository
with no supported source may not produce a usable analysis. Do not present a
missing or empty analysis as proof that application code passed.

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
  application checks are added. These integrations do not replace runtime,
  sandbox, credential, or platform verification.

## References

- [CodeRabbit configuration](https://docs.coderabbit.ai/reference/configuration)
- [CodeQL workflow options](https://docs.github.com/en/code-security/reference/code-scanning/workflow-configuration-options)
- [Sonar GitHub setup](https://docs.sonarsource.com/sonarqube-cloud/getting-started/github)
- [Sonar analysis methods](https://docs.sonarsource.com/sonarqube-cloud/analyzing-source-code/automatic-analysis)
- [Branch rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
