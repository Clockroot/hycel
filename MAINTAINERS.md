# Maintainers and issue triage

## Current ownership

- **Project owner / maintainer:** [`@aaf2tbz`](https://github.com/aaf2tbz) owns final decisions, release signing, security response, and repository administration until additional maintainers are explicitly added.
- **Code ownership:** `.github/CODEOWNERS` routes review requests to the current maintainer. It is a routing aid, not a promise that every PR will be reviewed on a schedule.
- Additional maintainers must be added here and to GitHub repository permissions before being treated as owners.

## Issue triage

Maintainers label new issues for area (`area:engine`, `area:renderer`, `area:platform`, `area:agent`, `area:docs`, `area:ci`), kind (`bug`, `feature`, `question`, `security`), and status (`needs-reproduction`, `confirmed`, `blocked`, `accepted`, `deferred`, `duplicate`) as appropriate. Use milestones for roadmap commitments; do not promise a response or release date without capacity.

Before accepting a bug, request the Hycel version, target OS/architecture, reproducible steps or minimal project, expected/actual result, and relevant logs with secrets removed. Do not ask users to post proprietary assets or personal data publicly.

Feature requests should explain the user problem, workflow, alternatives, and whether it fits [`docs/product-scope.md`](docs/product-scope.md). Close or defer requests outside 1.0 scope with a rationale rather than allowing the roadmap to grow by default.

## Security reports

Do not file exploitable vulnerability details in public issues. Follow [`SECURITY.md`](SECURITY.md) and use GitHub's private vulnerability reporting for this repository. Security issues take precedence over normal triage.

## Pull-request expectations

A maintainer may request tests, docs, platform evidence, a smaller patch, or an ADR for architectural changes. Reviewers must consider data safety, security, API/schema compatibility, and Windows/macOS/Linux behavior—not only whether a patch compiles locally. Release authority remains with the project owner until delegated here.
