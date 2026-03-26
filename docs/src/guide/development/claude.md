# Claude Code Integration

AI-assisted development with Claude Code, deeply integrated into the config.

## Overview

Claude Code is configured at three levels:
1. **Organization policy** — non-overridable deny list (security floor)
2. **Project settings** — repo-specific tool allowlist
3. **User preferences** — personal settings and default mode

## Agents

Specialized agents handle different tasks:
- **doc-writer** — keeps documentation in sync with code changes
- **test-runner** — runs the validation suite
- **scope** — scaffolds new scope modules
- And more, each with specific tools and memory

## Skills

Reusable workflows invoked with slash commands:
- `/ship` — ship a feature from worktree to main with validation
- `/docs-generate` — regenerate both documentation sites
- `/deploy` — build, validate, and deploy
- `/security` — run a comprehensive security review

## MCP Servers

Model Context Protocol servers provide Claude with live context about the codebase and development environment.

## Rules

Project rules in `.claude/rules/` encode hard-won lessons:
- Nix gotchas and pitfalls
- Config dependency chains
- Wrapper boundary decisions
- Platform design principles
- Security review process
- Testing strategy

## Further Reading

- [Technical Claude Details](../../claude/README.md) — full agent/skill/MCP reference
- [Security Model](../advanced/security.md) — the 3-level permissions model
