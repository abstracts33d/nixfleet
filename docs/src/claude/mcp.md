# MCP Servers

## Purpose

Model Context Protocol (MCP) servers provide Claude Code with external tool integrations. Configured at both project and user levels.

## Location

- `.mcp.json` -- project-level MCP servers
- `modules/scopes/dev/home.nix` (`programs.claude-code.mcpServers`) -- user-level servers

## Server Table

| Server | Level | Command | Purpose |
|--------|-------|---------|---------|
| `nixos` | Project | `npx mcp-nixos` | NixOS/HM/nix-darwin option lookups |
| `git` | User | `npx @anthropic/mcp-git` | Git operations |
| `github` | User | `npx @anthropic/mcp-github` | GitHub API (via `gh auth token`) |
| `filesystem` | User | `npx @anthropic/mcp-filesystem` | File system access to home dir |

## Configuration Details

### nixos (project)
Provides NixOS, Home Manager, and nix-darwin option documentation lookups. Available to all Claude sessions in this repo.

### github (user)
Uses a wrapper script that extracts the GitHub token from `gh auth token` at runtime, avoiding hardcoded tokens.

### filesystem (user)
Scoped to the user's home directory (`${hS.home}`).

## Links

- [Claude Overview](README.md)
- [Dev scope](../scopes/dev.md) (user MCP server definitions)
