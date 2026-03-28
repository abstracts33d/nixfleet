# Summary

[Introduction](README.md)

---

# Guide

- [Overview](guide/README.md)

## Getting Started

- [Quick Start](guide/getting-started/quick-start.md)
- [Installation](guide/getting-started/installation.md)
- [Day-to-Day Usage](guide/getting-started/daily-usage.md)

## Concepts

- [Why NixOS?](guide/concepts/why-nixos.md)
- [Declarative Configuration](guide/concepts/declarative.md)
- [The Scope System](guide/concepts/scopes.md)
- [Impermanence](guide/concepts/impermanence.md)
- [Secrets Management](guide/concepts/secrets.md)
- [Portable Environments](guide/concepts/portable.md)

## Desktop

- [Choosing a Desktop](guide/desktop/choosing.md)
- [Niri + Noctalia](guide/desktop/niri.md)
- [Theming with Catppuccin](guide/desktop/theming.md)

## Development

- [Dev Tools](guide/development/tools.md)
- [Claude Code Integration](guide/development/claude.md)
- [GitHub Workflow](guide/development/github-workflow.md)
- [Testing Your Config](guide/development/testing.md)
- [VM Testing](guide/development/vm-testing.md)

## Advanced

- [Adding a New Host](guide/advanced/new-host.md)
- [Adding a New Scope](guide/advanced/new-scope.md)
- [Cross-Platform Design](guide/advanced/cross-platform.md)
- [Security Model](guide/advanced/security.md)

---

# Reference

- [Architecture](architecture.md)

## Hosts

- [Overview](hosts/README.md)
  - [krach](hosts/krach.md)
  - [ohm](hosts/ohm.md)
  - [lab](hosts/lab.md)
  - [VM Hosts](hosts/vm/README.md)
    - [krach-qemu](hosts/vm/krach-qemu.md)
    - [qemu](hosts/vm/qemu.md)

## Scopes

- [Overview](scopes/README.md)
  - [base](scopes/base.md)
  - [impermanence](scopes/impermanence.md)
  - [NixFleet Agent](scopes/nixfleet-agent.md)
  - [NixFleet Control Plane](scopes/nixfleet-control-plane.md)

## Core Modules

- [Overview](core/README.md)
  - [nixos](core/nixos.md)
  - [darwin](core/darwin.md)

## Apps

- [Overview](apps/README.md)
  - [install](apps/install.md)
  - [build-switch](apps/build-switch.md)
  - [validate](apps/validate.md)
  - [docs](apps/docs.md)
  - [spawn-qemu](apps/spawn-qemu.md)
  - [spawn-utm](apps/spawn-utm.md)
  - [test-vm](apps/test-vm.md)
  - [rollback](apps/rollback.md)

## Testing

- [Test Pyramid](testing/README.md)
  - [Eval Tests](testing/eval-tests.md)
  - [VM Tests](testing/vm-tests.md)

## Claude Code Integration

- [Overview](claude/README.md)
  - [Scopes](claude/scopes.md)
  - [Permissions](claude/permissions.md)
  - [Agents](claude/agents.md)
  - [Skills](claude/skills.md)
  - [Hooks](claude/hooks.md)
  - [MCP](claude/mcp.md)
  - [Rules](claude/rules.md)

## Secrets Management

- [Overview](secrets/README.md)
  - [Bootstrap](secrets/bootstrap.md)
  - [WiFi](secrets/wifi.md)

---

# Business

- [Overview](business/README.md)

## Specs

- [mkFleet API Reference](business/specs/mk-fleet-api.md)

## Research

- [Two-Repo Split via flake-parts](business/research/two-repo-split-flake-parts.md)
- [Framework vs Overlay Separation](business/research/framework-vs-overlay-separation.md)
- [Client Needs per Tier](business/research/client-needs-per-tier.md)
