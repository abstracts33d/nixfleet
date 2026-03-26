# Claude Code Scopes

## Purpose

Instructions are organized in 3 scopes with different management layers, ensuring Claude Code receives consistent context whether on NixOS, macOS, or any project.

## Scope Table

| Scope | Location | Managed by | Content |
|-------|----------|-----------|---------|
| **User** | `~/.claude/CLAUDE.md` + `~/.claude/rules/` | HM `programs.claude-code` in `scopes/dev/home.nix` | Personal preferences, git workflow, doc maintenance |
| **Organization** | `/etc/claude-code/CLAUDE.md` | NixOS `environment.etc` in `core/nixos.nix` | Nix development policies (NixOS only) |
| **Project** | `CLAUDE.md` + `.claude/rules/` | Git-tracked in repo | Repo architecture, rules, patterns |

## User Scope

Managed declaratively via `programs.claude-code.memory` and `programs.claude-code.rules` in the HM module. Contains:
- Profile (experience level, GitHub identity, theme preference)
- Communication preferences (language, conciseness)
- Autonomy guidelines
- Workflow rules (git-workflow, docs-maintenance, workflow-preferences)

## Organization Scope

Written to `/etc/claude-code/settings.json` by NixOS `environment.etc`. Contains the non-overridable deny list. Only applies on NixOS (not Darwin).

## Project Scope

Git-tracked files:
- `CLAUDE.md` -- architecture overview, commands, module tree
- `.claude/rules/` -- 8 rule files for specific domains
- `.claude/settings.json` -- project allow list and hooks

## Auto-Memory

`~/.claude/projects/*/memory/` stores ephemeral session notes. Path-encoded per project. Not shared across machines (different path encoding).

## Links

- [Claude Overview](README.md)
- [Permissions](permissions.md)
- [Rules](rules.md)
