# dev

## Purpose

Development environment: Docker, language runtimes, dev CLI tools, and Claude Code AI assistant with full declarative configuration (settings, memory, rules, MCP servers).

## Location

- `modules/scopes/dev/nixos.nix` -- NixOS: Docker, PostgreSQL, system persist
- `modules/scopes/dev/home.nix` -- HM: direnv, mise, Claude Code, dev packages

## Configuration

**Gate:** `isDev`

### NixOS module

- Docker with json-file log driver
- PostgreSQL
- VS Code (system-level)
- Impermanence: `/var/lib/docker`, `/var/lib/postgresql`

### HM module

**Programs:**
- `direnv` (with nix-direnv)
- `mise` (runtime version manager)
- `claude-code` (AI assistant, declaratively configured)

**Claude Code config:**
- `defaultMode: bypassPermissions` (deny list at org level)
- Plugins: superpowers, code-simplifier
- User memory with profile, preferences, workflow
- User rules: git-workflow, docs-maintenance, workflow-preferences
- MCP servers: git, github (via `gh auth token`), filesystem

**Packages (generic framework defaults):**
- Dev CLI: gcc, shellcheck
- Nix: nix-tree, alejandra, deadnix
- Containers: docker, docker-compose

> **Org-specific packages** (e.g. act, difftastic, databases, Node.js, Python, spell-check) are added via org hmModules in `fleet.nix`, not in the framework scope. This keeps the framework portable.

### Impermanence persist paths (HM)

`.docker`, `.npm`, `.cargo`, `.cache/pip`, `.cache/yarn`, `.local/share/mise`, `.cache/mise`, `.cache/direnv`, `.local/share/direnv`, `.config/pgcli`, `.claude`

## Dependencies

- Depends on: hostSpec `isDev` flag
- Claude Code unfree: must be in HM module (not wrappers/perSystem)

## Links

- [Scope Overview](README.md)
- [Claude Code integration](../claude/README.md)
