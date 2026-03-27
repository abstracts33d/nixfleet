# Development Workflow

See `.claude/rules/git-workflow.md` for the authoritative git/PR/shipping rules.

This file covers supplementary workflow knowledge (parallelism, doc sync, issue tracking).

## CI Pipeline

GitHub Actions (`.github/workflows/ci.yml`):
1. `nix fmt -- --fail-on-change` (formatting check)
2. `nix run .#validate` (builds all hosts + packages + eval tests)

## Parallelism Requirement

When dispatching subagents for any plan:
1. Analyze task dependencies -- which tasks share files?
2. If 2+ tasks are independent, dispatch them in parallel (multiple Agent calls in a SINGLE message)
3. Never batch independent tasks into one sequential agent

## Documentation Sync (merge-blocking)

Every code change must update ALL affected doc trees:
- `CLAUDE.md` -- framework AI context
- `README.md` -- user-facing
- `docs/src/` -- technical reference (mdbook)
- `docs/guide/` -- user guide (mdbook)

Business docs (`docs/nixfleet/`) are updated when business-relevant, not merge-blocking.

## Issue Tracking

- GitHub Issues with labels for scope, urgency, and type
- GitHub Projects board (#1) with columns: Backlog, Ready, In Progress, In Review, Done
- Board transitions via `scripts/gh-issue-helper.sh` functions
- Phase status is tracked in CLAUDE.md and the project board

## Commit Conventions

- Prefix: `feat:`, `fix:`, `refactor:`, `docs:`, `chore:`, `test:`, `infra:`
- Concise subject line (<72 chars)
- Body explains "why" not "what"
- Reference issues: `Closes #XX` or `Refs #XX`

## Pre-merge Checklist

1. CI passes (formatting + validate)
2. All doc trees updated
3. Tests cover new behavior (eval at minimum, VM if runtime)
4. PR description follows template
5. Issues linked and board updated
