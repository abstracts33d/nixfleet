# Claude Code Agents

## Purpose

16 specialized agents for different tasks, each with a defined model, role, and memory scope. Agents are dispatched by skills or by the orchestration layer.

## Location

- `.claude/agents/` — agent definition files

## Agent Table

| Agent | Model | Role | Tools |
|-------|-------|------|-------|
| `architect` | inherit | Cross-cutting architecture decisions, ADRs | Read, Grep, Glob |
| `code-reviewer` | sonnet | Code quality, conventions, dependency chains | Read, Grep, Glob, Bash |
| `config-manager` | sonnet | Claude Code infrastructure maintenance | Read, Grep, Glob, Edit, Write |
| `devops` | sonnet | CI/CD pipelines, git hooks, build caching | Read, Grep, Glob, Bash, Edit, Write |
| `doc-writer` | sonnet | Documentation maintenance after code changes | Read, Grep, Glob, Edit, Write |
| `docs-assessor` | sonnet | Documentation quality and coherence review | Read, Grep, Glob |
| `fleet-ops` | inherit | Day-2 operations (deploy, rollback, secrets) | Read, Grep, Glob, Bash |
| `integration-tester` | inherit | End-to-end NixFleet workflows | Read, Grep, Glob, Bash |
| `nix-expert` | inherit | Nix build errors, module wiring, architecture | Read, Grep, Glob, Bash, Edit, Write |
| `plan-reviewer` | inherit | Validate plan coherence against spec | Read, Grep, Glob, Write, Edit |
| `plan-writer` | inherit | Implementation plans from specs | Read, Grep, Glob, Edit, Write |
| `product-analyst` | sonnet | Market research, client needs, feature priorities | Read, Grep, Glob, WebSearch, WebFetch |
| `rust-expert` | inherit | Rust compilation, async patterns, agent/CP arch | Read, Grep, Glob, Bash, Edit, Write |
| `security-reviewer` | sonnet | Security audit with timestamped reports | Read, Grep, Glob, Write |
| `spec-writer` | inherit | Design specifications from brainstorming | Read, Grep, Glob, Edit, Write |
| `test-runner` | haiku | Run tests, analyze failures, suggest fixes | Read, Grep, Bash |

## Design Principles

- **Cost-optimized:** Read-only/analysis agents use `sonnet`, execution agents use `inherit` (caller's model)
- **All use project memory:** Agents share the project's CLAUDE.md, rules, and settings
- **Local enforcement:** Each agent contains 1-2 lines specifying which skills it must use
- **Composable:** Skills orchestrate multiple agents in sequence or parallel

## Links

- [Claude Overview](README.md)
- [Skills](skills.md) (orchestrate agents)
