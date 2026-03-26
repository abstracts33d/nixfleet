# GitHub Workflow

## Issue Tracking

All work items are tracked as [GitHub Issues](https://github.com/abstracts33d/nixfleet/issues) on the NixFleet project board.

### Labels

Issues are categorized with labels:
- **Scope** (`scope:core`, `scope:enterprise`, etc.) — which part of the config
- **Type** (`feature`, `bug`, `refactor`, `docs`, `infra`) — what kind of work
- **Impact** (`impact:critical` to `impact:low`) — business value
- **Urgency** (`urgency:now`, `urgency:soon`, `urgency:later`) — when
- **Phase** (`phase:S0` to `phase:S8`) — NixFleet roadmap phase

### Creating Issues

```bash
# Quick create
gh issue create -R abstracts33d/nixfleet

# Using the helper
source scripts/gh-issue-helper.sh
gh_create_issue "title" "body" "scope:core,feature,impact:medium" "S0: Foundation"
```

## Pull Requests

All changes go through PRs — direct push to `main` is blocked.

### Branch Naming

Use `<type>/<description>`:
- `feat/vpn-scope`
- `fix/portal-warning`
- `docs/enterprise-pages`

### PR Template

PRs auto-fill with a template asking for summary, linked issue (`Closes #XX`), and test plan.

### CI Checks

Every PR runs:
1. **Format check** — `nix fmt --fail-on-change`
2. **Validate** — `nix flake check --no-build` (eval tests)

Both must pass before merge. PRs are squash-merged (one clean commit per feature).

## Claude Integration

Claude skills are integrated with the issue tracker:
- `/suggest` reads open issues to propose next work
- `/plan-and-execute` creates tracking issues for new features
- `/ship` closes linked issues on merge
- `/review` posts review summaries to issues
- `/scope` creates tracking issues for new scopes
