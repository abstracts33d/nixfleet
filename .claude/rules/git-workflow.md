# Git Workflow (nixfleet specifics)

Extends the generic git workflow rules from the claude-core plugin.

## Hooks

- **Pre-commit (git):** `nix fmt --fail-on-change` (~2s)
- **Pre-push (git):** format + eval tests + cargo test (~15s)
- **Pre-commit (Claude):** format check before `git commit`
- **Pre-push (Claude):** eval tests before `git push`
- **Guard:** block destructive commands, force push to main

## CI (GitHub Actions)

- Runs on PRs to `main` only
- Format check + eval tests (not full host builds -- too slow)
- Full validation (`nix run .#validate`) is manual or pre-push hook
