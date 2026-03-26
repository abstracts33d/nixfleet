# Claude Code Permissions

## Purpose

3-level permission model where higher levels cannot override lower levels. The org deny list is the security floor.

## Location

- `/etc/claude-code/settings.json` -- org level (NixOS `environment.etc` in `core/nixos.nix`)
- `.claude/settings.json` -- project level (git-tracked)
- `~/.claude/settings.json` -- user level (HM `programs.claude-code.settings`)

## Level 1: Org (Managed Policy)

Non-overridable deny list written to `/etc/claude-code/settings.json`. Blocks:
- **Destructive ops:** `rm -rf`, `rm -r`, `dd`, `mkfs`, `shred`
- **Privilege escalation:** `sudo`, `pkexec`, `doas`, `su`
- **Dangerous git:** `push --force`, `push -f`, `reset --hard`, `clean -fd`
- **Nix store manipulation:** `nix-store --delete`, `nix store delete`

Only applies on NixOS (Darwin has no `environment.etc`).

## Level 2: Project

Allow list in `.claude/settings.json`:
- `nix *`, `git *`, `alejandra *`, `deadnix *`, `ssh *`, `scp *`

Plus hook definitions (PostToolUse, PreToolUse, SessionStart, Stop).

## Level 3: User

Via HM `programs.claude-code.settings`:
- Additional allow patterns (find, ls, git, rails, bundle, rubocop)
- `defaultMode: bypassPermissions` (auto-approves non-denied commands)
- `skipDangerousModePermissionPrompt: true`

## Security Properties

- User `bypassPermissions` cannot override the org deny list
- Org deny list is **only on NixOS** -- Darwin users get project + user levels only
- The deny list covers the most dangerous operations but is not exhaustive

## Links

- [Claude Overview](README.md)
- [NixOS core](../core/nixos.md) (org deny list definition)
- [Dev scope](../scopes/dev.md) (user settings definition)
