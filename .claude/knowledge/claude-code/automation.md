# Claude Code Automation

Knowledge about the agent/skill/MCP automation layer.

## Memory Scopes

| Scope | Location | Managed by | Content |
|-------|----------|-----------|---------|
| **User** | `~/.claude/CLAUDE.md` + `~/.claude/rules/` | HM `programs.claude-code` | Personal preferences, workflow |
| **Organization** | `/etc/claude-code/CLAUDE.md` | NixOS `environment.etc` | Nix policies (NixOS only) |
| **Project** | `CLAUDE.md` + `.claude/rules/` | Git-tracked | Repo architecture, rules, patterns |

Auto-memory (`~/.claude/projects/*/memory/`) is for ephemeral session notes. Durable knowledge lives in the three scopes above, plus the `knowledge/` tree.

## Agent Levels

**User interactions → high-level skills ONLY.** Low-level agents are dispatched BY skills, not by users.

### High-level (user-facing skills)
| Skill | User says | Dispatches |
|-------|-----------|------------|
| `/suggest` | "what should I do?" | code-reviewer + security + nix-expert + docs-assessor |
| `/audit` | "audit the codebase" | config-manager → security → code → architect → product |
| `/feature` | "add feature X" | analyst → architect → spec → plan → implement → review |
| `/review` | "review the code" | code-reviewer + security + docs-assessor |
| `/ship` | "ship this" | test-runner → doc-writer → config-manager → docs-assessor |
| `/health` | "check health" | config-manager + test-runner + devops |
| `/onboard` | "add org X" | analyst → architect → nix → fleet-ops → docs |
| `/incident` | "X is broken" | fleet-ops → nix → security → architect |
| `/plan-and-execute` | "implement X" | research → spec → plan → execute → ship |

### Low-level (specialist agents — 15 total)
| Agent | Model | Role |
|-------|-------|------|
| `nix-expert` | inherit | Build errors, architecture, module wiring |
| `rust-expert` | inherit | Rust workspace, cargo, async, agent/CP |
| `security-reviewer` | sonnet | Security audit (read-only) |
| `code-reviewer` | sonnet | Code quality, conventions |
| `test-runner` | haiku | Run tests, analyze failures |
| `doc-writer` | haiku | Update ALL doc trees |
| `docs-assessor` | sonnet | Documentation coherence (read-only) |
| `spec-writer` | inherit | Write design specs |
| `plan-writer` | inherit | Write implementation plans |
| `config-manager` | sonnet | Claude Code infra, knowledge tree |
| `architect` | inherit | Cross-cutting architecture (read-only) |
| `fleet-ops` | inherit | Day-2 operations, deploy, rollback |
| `product-analyst` | sonnet | Client needs, tiers, competitive analysis |
| `devops` | sonnet | CI/CD, hooks, pipeline |
| `integration-tester` | inherit | E2E agent↔CP tests |

## Skills (17 total)

| Skill | Agents dispatched |
|-------|-------------------|
| `/suggest` | code-reviewer + security-reviewer + nix-expert + docs-assessor (parallel) |
| `/review` | code-reviewer + security-reviewer + docs-assessor (parallel) |
| `/ship` | test-runner + doc-writer + config-manager + docs-assessor |
| `/security` | security-reviewer |
| `/scope` | doc-writer + test-runner |
| `/diagnose` | nix-expert or test-runner |
| `/plan-and-execute` | spec-writer + plan-writer + test-runner + doc-writer |
| `/docs-generate` | doc-writer (both doc trees) |
| `/audit` | config-manager → security → code + rust (parallel) → architect → product |
| `/feature` | analyst → architect → spec → plan → implement → review → ship |
| `/onboard` | analyst → architect → nix → fleet-ops → docs |
| `/incident` | fleet-ops → nix → security → architect |
| `/health` | parallel: config-manager + test-runner + devops |
| `/batch` | N subagents in worktrees |
| `/secrets` | nix-expert |
| `/assess-docs` | docs-assessor |
| `/deploy` | test-runner |

## MCP Servers

- **Project** (`.mcp.json`): `mcp-nixos` (NixOS option lookups), `rust-analyzer` (Rust navigation)
- **User** (HM `mcpServers`): `git`, `github` (via `gh auth token`), `filesystem`

## Permissions & Configuration

See `knowledge/claude-code/configuration.md` for the 3-level permissions model (org/project/user) and file hierarchy. These are NixOS-managed via `core/nixos.nix` (org) and `scopes/dev/home.nix` (user).

## Knowledge Tree

Agents have scoped knowledge via `knowledge:` field in frontmatter. Each agent loads only relevant domain files.

Knowledge domains: `nix/`, `nixfleet/`, `security/`, `testing/`, `languages/`, `platform/`, `claude-code/`.

Agents propose new knowledge via `_proposals/` directory → human review → integration.
