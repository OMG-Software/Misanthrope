# 0004: Require pull requests and repository analysis

- Status: Accepted
- Date: 2026-11-08

## Context

The project owner requires CodeRabbit, CodeQL, and Sonar and wants main protected
from direct pushes. The repository currently has documentation and no application
scaffold. GitHub review apps already cover all organization repositories.

## Decision

Require pull requests for main with no bypass actors, and block force-pushes
and branch deletion. Publish this setup through a pull request.

Use CodeRabbit for automatic reviews and CodeQL for Actions now, with Rust and
JavaScript/TypeScript analysis selected when their source appears. Do not add
placeholder application code merely to make analyzers run.

Use Sonar CI analysis with the requested repository secret. Keep Automatic
Analysis disabled. Document current analysis scope, missing credentials, and
no-source limitations in [repository checks](../repository-checks.md).

The initial PR-only rule adds no mandatory human approvals or named status
checks. Review those gates separately when the intended checks are verified.

## Alternatives considered

- Let administrators bypass pull requests: this would permit direct pushes and
  weaken the requested PR-only path.
- Require Rust analysis before source exists: this would fail because there is
  no application manifest or source, or encourage a misleading placeholder.
- Enable Sonar automatic and CI analysis together: the service rejects the
  conflicting methods.
- Require unavailable external checks immediately: this can block all merges
  without establishing any useful verification.

## Consequences

Main updates require GitHub pull-request merges. App installation and config
files alone are not passing analyses. Fork pull requests do not receive configured
repository secrets, but their actions can still access a limited GITHUB_TOKEN.
Additional app build, test, lint, and coverage checks
remain work for the implementation scaffold.
