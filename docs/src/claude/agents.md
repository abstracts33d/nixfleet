# Claude Code Agents

## Purpose

7 specialized agents for different tasks, each with a defined model, role, and memory scope. Agents are dispatched by skills or invoked directly.

## Location

- `.claude/agents/` -- agent definition files

## Agent Table

| Agent | Model | Role | Memory |
|-------|-------|------|--------|
| `nix-expert` | inherit | Build errors, architecture, module wiring | project |
| `security-reviewer` | sonnet | Security audit (read-only) | project |
| `code-reviewer` | sonnet | Code quality, conventions, dependency chains | project |
| `test-runner` | haiku | Run tests, analyze failures | project |
| `doc-writer` | haiku | Update CLAUDE.md, README.md, TODO.md | project |
| `spec-writer` | inherit | Write design specs from brainstorming | project |
| `plan-writer` | inherit | Write implementation plans from specs | project |

## Agent Design

- **Cost-optimized:** Read-only/analysis agents use `sonnet`, execution agents use `haiku`, architecture agents use `inherit` (caller's model)
- **All use project memory:** Agents share the project's CLAUDE.md, rules, and settings
- **Composable:** Skills orchestrate multiple agents in sequence or parallel

## Links

- [Claude Overview](README.md)
- [Skills](skills.md) (orchestrate agents)
