# NixFleet User Guide

Manage your NixOS fleet declaratively — organizations, roles, and hosts defined as code.

This guide explains NixFleet concepts, architecture, and workflows. For technical reference, see the [Reference](../architecture.md) section. For the API, see [mkFleet API](../business/specs/mk-fleet-api.md).

## What is NixFleet?

An open-core framework for managing fleets of NixOS machines. Define your organization, assign roles, and deploy to 1 or 1000 machines — with reproducible builds, instant rollback, and zero config drift.

The **reference fleet** (`abstracts33d` organization) demonstrates all features with **14 hosts** from one file (`modules/fleet.nix`):
- **4 physical machines** — krach, ohm, lab (NixOS) + aether (macOS)
- **4 test VMs** — QEMU + UTM mirrors
- **3 batch hosts** — simulated edge fleet via `mkBatchHosts`
- **3 test matrix hosts** — role x platform CI validation via `mkTestMatrix`

All managed declaratively via `mkFleet` — define your org, assign roles, rebuild, done.

## Key Features

- **Scope-based architecture** — features self-activate based on host flags
- **Impermanent root** — ephemeral filesystem, only persist what matters
- **Encrypted secrets** — agenix for SSH keys, passwords, WiFi
- **Automated testing** — eval tests, VM tests, one-command validation
- **Claude Code integration** — agents, skills, MCP servers, automated workflows
- **Cross-platform** — same config drives NixOS and macOS
- **Portable shells** — `nix run .#shell` works on any machine with Nix installed

## How to Read This Guide

- **New to NixOS?** Start with [Why NixOS?](concepts/why-nixos.md) then [Quick Start](getting-started/quick-start.md)
- **Setting up your fleet?** Go to [Installation](getting-started/installation.md)
- **Day-to-day fleet ops?** See [Daily Usage](getting-started/daily-usage.md)
- **Adding hosts or scopes?** Read [Adding a New Host](advanced/new-host.md), [New Scope](advanced/new-scope.md)
- **GitHub workflow?** See [GitHub Workflow](development/github-workflow.md)

## Quick Commands

```sh
# Rebuild after changes
nix run .#build-switch

# Portable dev shell on any machine
nix run .#shell

# Run all validations
nix run .#validate

# Serve documentation locally
nix run .#docs
```
