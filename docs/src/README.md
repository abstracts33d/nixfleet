# NixFleet Documentation

Declarative NixOS fleet management — organizations, roles, and hosts defined as code.

This documentation is organized into three sections:

## [Guide](guide/README.md)

Getting started, concepts, desktop setup, development workflow, and advanced topics. Start here if you are new to NixFleet.

## [Reference](architecture.md)

Technical reference for hosts, scopes, core modules, wrappers, apps, testing, Claude Code integration, and secrets management.

## [Business](business/README.md)

Roadmap, API specs, research documents, market analysis, and strategic planning.

## Quick Commands

```sh
# Rebuild after changes
nix run .#build-switch

# Portable dev shell on any machine
nix run .#shell

# Run all validations
nix run .#validate

# Serve this documentation locally
nix run .#docs
```
