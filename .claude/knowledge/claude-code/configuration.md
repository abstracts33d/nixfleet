# Claude Code Configuration

## File Structure

| File | Scope | Content |
|------|-------|---------|
| `CLAUDE.md` | Project | Routing, critical rules, commands |
| `.claude/rules/*.md` | Project | Enforcement rules (blocking) |
| `.claude/knowledge/` | Project | Domain knowledge (context) |
| `.claude/agents/*.md` | Project | Agent definitions with knowledge scoping |
| `.claude/skills/` | Project | Workflow orchestration |
| `.claude/settings.json` | Project | Permissions, hooks |

## Hooks

| Event | Hook | What it does |
|-------|------|-------------|
| PostToolUse (Edit/Write) | `format-nix.sh` | Auto-format .nix files |
| PostToolUse (Edit/Write) | `check-config-deps.sh` | Remind about dependency chains |
| PostToolUse (Edit/Write) | `check-docs-tree.sh` | Remind about doc updates |
| PreToolUse (Bash) | `pre-git-commit.sh` | Gate git commits |
| PreToolUse (Bash) | `pre-git-push.sh` | Gate git pushes |
| PreToolUse (Bash) | `guard-destructive.sh` | Block dangerous commands |
| SessionStart | `session-context.sh` | Load session context |

## Knowledge vs Rules

- **Rules** = things that MUST happen (enforcement, blocking)
  - `config-dependencies.md`, `git-workflow.md`, `wrapper-boundary.md`, `testing.md`
- **Knowledge** = things that help understanding (context, patterns)
  - Everything in `knowledge/` — gotchas, architecture, patterns

## Agent Scoping

Agents have a `knowledge:` frontmatter field listing their domains.
Only the listed knowledge files are relevant to that agent's work.
This prevents context pollution (nix-expert doesn't need Rust patterns).

## MCP Servers

- **Project** (`.mcp.json`): `mcp-nixos`, `rust-analyzer`
