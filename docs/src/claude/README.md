# Claude Code Integration

## Purpose

Comprehensive Claude Code AI assistant integration, declaratively managed by Nix. Includes a 3-scope instruction system, 3-level permissions model, 7 agents, 10 skills, 7 hooks, MCP servers, and 8 project rules.

## Location

- `.claude/` -- project-level Claude configuration
- `modules/scopes/dev/home.nix` -- user-level Claude settings via HM
- `modules/core/nixos.nix` -- org-level deny list
- `.mcp.json` -- project MCP servers

## Components

| Component | Count | Description |
|-----------|-------|-------------|
| [Scopes](scopes.md) | 3 | Instruction delivery (user, org, project) |
| [Permissions](permissions.md) | 3 levels | Security model (org deny, project allow, user mode) |
| [Agents](agents.md) | 7 | Specialized AI agents |
| [Skills](skills.md) | 10 | Orchestration workflows |
| [Hooks](hooks.md) | 7 | Automation triggers |
| [MCP](mcp.md) | 4 servers | External tool integrations |
| [Rules](rules.md) | 8 | Project behavior rules |

## Architecture

Claude Code configuration is **declaratively managed by Nix** across three layers:
1. **Org policy** (NixOS `environment.etc`) -- security floor, cannot be bypassed
2. **Project config** (git-tracked `.claude/`) -- repo-specific tools, hooks, agents
3. **User preferences** (HM `programs.claude-code`) -- personal settings, memory, MCP servers

## Links

- [Dev scope](../scopes/dev.md) (where Claude Code HM config lives)
- [NixOS core](../core/nixos.md) (where org deny list lives)
