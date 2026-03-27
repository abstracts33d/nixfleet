# Claude Code Skills

## Purpose

14 active orchestration skills that compose agents into workflows. Invoked via slash commands (e.g., `/ship`, `/review`). 3 additional skills exist but are deprecated.

## Location

- `.claude/skills/` — skill definition files (each in `<name>/SKILL.md`)

## Active Skills

| Skill | Agents Dispatched | Description |
|-------|-------------------|-------------|
| `/assess-docs` | docs-assessor | Meta-level documentation review for coherence, staleness, and cross-document consistency |
| `/audit` | config-manager, security-reviewer, code-reviewer, architect, product-analyst | Full codebase audit — config health, security, code quality, architecture, product gaps |
| `/diagnose` | nix-expert or test-runner | Analyze build/test failures and propose fixes |
| `/docs-generate` | doc-writer | Regenerate both documentation trees from current codebase state |
| `/feature` | product-analyst, architect, spec-writer, plan-writer, code-reviewer | End-to-end feature flow from client need to shipped code |
| `/health` | config-manager, test-runner | Quick health check — config, tests, CI, format, issues |
| `/incident` | fleet-ops, nix-expert, security-reviewer, architect | Incident response — diagnose fleet issue, assess security impact, recommend fix |
| `/onboard` | product-analyst, architect, nix-expert, fleet-ops, doc-writer | Onboard a new organization onto NixFleet |
| `/plan-and-execute` | spec-writer, plan-writer, test-runner, doc-writer | End-to-end feature implementation with research, spec, plan, execute phases |
| `/review` | code-reviewer, security-reviewer, docs-assessor | Parallel code + security + docs review of current changes |
| `/scope` | doc-writer, test-runner | Scaffold a new NixOS scope with tests and docs |
| `/secrets` | nix-expert | Manage agenix secrets across fleet and secrets repos |
| `/security` | security-reviewer | Full security audit with timestamped report |
| `/ship` | test-runner, doc-writer, docs-assessor | Ship feature with validation — tests, docs, PR |
| `/suggest` | code-reviewer, security-reviewer, nix-expert, docs-assessor | Analyze repo state and suggest prioritized improvements |

## Deprecated Skills

| Skill | Replacement | Reason |
|-------|-------------|--------|
| `/batch` | `superpowers:dispatching-parallel-agents` | Superseded by superpowers parallel dispatch |
| `/deploy` | Manual `nix run .#build-switch` | User-only, disable-model-invocation — not an AI workflow |

## Workflow Examples

**Feature development:**
1. `/plan-and-execute` — design + plan
2. Implement in worktree
3. `/review` — code + security review
4. `/ship` — tests + docs
5. `/suggest` — catch forgotten items

**Troubleshooting:**
- `/diagnose` — dispatches nix-expert for build errors or test-runner for test failures

**New client:**
- `/onboard` — full analysis, architecture, fleet setup, documentation

## Links

- [Claude Overview](README.md)
- [Agents](agents.md)
