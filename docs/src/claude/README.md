# Claude Code Integration

## Purpose

Comprehensive Claude Code AI assistant integration, declaratively managed by Nix. Includes a 3-scope instruction system, 3-level permissions model, 15 agents, 17 skills, 8 rules, 8 hooks, 2 MCP servers, and 23 knowledge files.

## Location

- `.claude/` — project-level Claude configuration (agents, skills, rules, knowledge, hooks)
- `modules/scopes/dev/home.nix` — user-level Claude settings via HM (in fleet overlay)
- `modules/core/nixos.nix` — org-level deny list
- `.mcp.json` — project MCP servers

## Components

| Component | Count | Description |
|-----------|-------|-------------|
| [Scopes](scopes.md) | 3 | Instruction delivery (user, org, project) |
| [Permissions](permissions.md) | 3 levels | Security model (org deny, project allow, user mode) |
| [Agents](agents.md) | 15 | Specialized AI agents |
| [Skills](skills.md) | 17 | Orchestration workflows |
| [Hooks](hooks.md) | 8 | Automation triggers |
| [MCP](mcp.md) | 2 servers | External tool integrations (nixos, rust-analyzer) |
| [Rules](rules.md) | 8 | Project behavior rules |
| Knowledge | 23 files | Contextual knowledge across 7 domains |

## Architecture

Claude Code configuration is **declaratively managed by Nix** across three layers:
1. **Org policy** (NixOS `environment.etc`) — security floor, cannot be bypassed
2. **Project config** (git-tracked `.claude/`) — repo-specific tools, hooks, agents
3. **User preferences** (HM `programs.claude-code`) — personal settings, memory, MCP servers

## Enforcement Model

- **Principles** live in CLAUDE.md (~10 lines) — skill-first, parallel, verify-before-done
- **Skill dispatch table** in CLAUDE.md — maps user intent to skills
- **Local enforcement** in each agent's prompt — "use TDD before code", "verify before claiming done"
- **No monolithic enforcement rule** — distributed across CLAUDE.md + agents

## Links

- [NixOS core](../core/nixos.md) (where org deny list lives)
