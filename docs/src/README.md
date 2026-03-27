# NixFleet Documentation

Declarative NixOS fleet management — organizations, roles, and hosts defined as code.

This documentation is organized into three sections:

## [Guide](guide/README.md)

Getting started, concepts, desktop setup, development workflow, and advanced topics. Start here if you are new to NixFleet.

## [Reference](architecture.md)

Technical reference for hosts, scopes, core modules, apps, testing, Claude Code integration (15 agents, 17 skills, 8 hooks, 8 rules, 23 knowledge files), and secrets management.

## [Business](business/README.md)

Roadmap, API specs, research documents, market analysis, and strategic planning.

## Quick Commands

```sh
# Rebuild after changes
nix run .#build-switch

# Run all validations
nix run .#validate

# Serve this documentation locally
nix run .#docs
```
