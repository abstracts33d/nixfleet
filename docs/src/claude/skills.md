# Claude Code Skills

## Purpose

10 orchestration skills that compose agents into workflows. Invoked via slash commands (e.g., `/ship`, `/review`).

## Location

- `.claude/skills/` -- skill definition files (each in `<name>/SKILL.md`)

## Skill Table

| Skill | Trigger | Agents Dispatched | Description |
|-------|---------|-------------------|-------------|
| `/ship` | User | test-runner, doc-writer | Run tests then update docs |
| `/review` | User | code-reviewer + security-reviewer (parallel) | Dual code + security review |
| `/security` | User | security-reviewer | Security audit |
| `/deploy` | User only | test-runner | Run deployment validation |
| `/batch` | User | N subagents in worktrees | Parallel task execution |
| `/diagnose` | User | nix-expert or test-runner | Troubleshoot build/test failures |
| `/scope` | User | doc-writer, test-runner | Add new scope with docs + tests |
| `/suggest` | User | code-reviewer + security-reviewer + nix-expert (parallel) | Scan for improvements |
| `/secrets` | User | nix-expert | Manage agenix secrets |
| `/plan-and-execute` | User | spec-writer, plan-writer | Design spec then implementation plan |

## Workflow Examples

**Feature development:**
1. `/plan-and-execute` -- design + plan
2. Implement in worktree
3. `/review` -- code + security review
4. `/ship` -- tests + docs
5. `/suggest` -- catch forgotten items

**Troubleshooting:**
- `/diagnose` -- dispatches nix-expert for build errors or test-runner for test failures

## Links

- [Claude Overview](README.md)
- [Agents](agents.md)
