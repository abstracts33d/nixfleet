# Claude Code Hooks

## Purpose

7 automation hooks that run at specific lifecycle points. Defined in `.claude/settings.json` and implemented as shell scripts in `.claude/hooks/`.

## Location

- `.claude/settings.json` -- hook registration
- `.claude/hooks/` -- hook scripts

## Hook Table

| Hook | Trigger | Script | Description |
|------|---------|--------|-------------|
| format-nix | PostToolUse (Edit/Write) | `format-nix.sh` | Auto-format .nix files with alejandra |
| check-config-deps | PostToolUse (Edit/Write) | `check-config-deps.sh` | Remind about config dependency chains |
| pre-git-commit | PreToolUse (Bash) | `pre-git-commit.sh` | Gate git commits with format check |
| pre-git-push | PreToolUse (Bash) | `pre-git-push.sh` | Gate git push with full validation (600s timeout) |
| guard-destructive | PreToolUse (Bash) | `guard-destructive.sh` | Block destructive commands |
| session-context | SessionStart | `session-context.sh` | Load context at session start |
| doc-sync-check | Stop | `doc-sync-check.sh` | Check docs are in sync before session ends |

## Lifecycle

```
SessionStart -> session-context.sh
  |
  v
PreToolUse (Bash) -> pre-git-commit.sh, pre-git-push.sh, guard-destructive.sh
  |
  v
[tool runs]
  |
  v
PostToolUse (Edit/Write) -> format-nix.sh, check-config-deps.sh
  |
  v
Stop -> doc-sync-check.sh
```

## Links

- [Claude Overview](README.md)
- [Permissions](permissions.md)
