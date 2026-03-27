# Git Workflow (HARD RULES)

These rules are non-negotiable. Violations break team trust and workflow.

## Branch Strategy

- **NEVER commit directly to main** — always use feature branches
- **Branch from main** (or from a local branch if iterating)
- **Naming:** `feat/`, `fix/`, `refactor/`, `docs/`, `infra/` prefix + description
- **Keep branches local** until ready to push — minimize GitHub API noise
- **One feature per branch** — don't mix unrelated changes

## PR Workflow

- **PRs required** for all changes to `main`
- **Squash-merge only** (enforced in repo settings)
- **Branches auto-delete** on merge
- **CI must pass** before merge (format + eval tests)
- **Include `Closes #XX`** in PR body to auto-close issues

## Shipping Convention

1. Work on feature branch locally
2. When ready: **STOP and present** — show branch summary, files changed, test results
3. **Ask:** "Review OK, can I ship?" — wait for explicit confirmation
4. Only then: push branch + create PR
5. **NEVER merge** — present PR URL, user merges manually on GitHub
6. After user merges: clean up local branch

## What NEVER to Do

- Push directly to main
- Merge PRs automatically (even with `--admin`)
- Force push to main (except explicit user request for history rewrite)
- Skip the "review OK?" checkpoint
- Create PRs without presenting changes first
- Use `--no-verify` to skip hooks without good reason

## Hooks

- **Pre-commit (git):** `nix fmt --fail-on-change` (~2s)
- **Pre-push (git):** format + 3 eval tests + cargo test (~15s)
- **Pre-commit (Claude):** format check before `git commit`
- **Pre-push (Claude):** eval tests before `git push`
- **Guard:** block destructive commands, force push to main

---

## Org-Specific

> The section below is specific to the `abstracts33d` reference fleet CI setup.

### CI (GitHub Actions)

- Runs on PRs to `main` only
- Format check + eval tests (not full host builds — too slow)
- Full validation (`nix run .#validate`) is manual or pre-push hook
