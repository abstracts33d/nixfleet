# CLAUDE.md

NixFleet is a declarative NixOS fleet management framework. This repo contains the framework API, modules, Rust agent/control-plane, and CLI.

## Project Structure

| Location | Content |
|----------|---------|
| `modules/_shared/lib/` | Framework API (mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix) |
| `modules/_shared/` | hostSpec options, disk templates, keys |
| `modules/core/` | Core deferred modules (nixos.nix, darwin.nix, home.nix) |
| `modules/scopes/` | Scope modules (catppuccin, graphical, dev, desktop, enterprise, nixfleet) |
| `modules/wrappers/` | Portable composites (shell.nix, terminal.nix) |
| `modules/tests/` | Test infrastructure (eval.nix, vm.nix, integration/) |
| `agent/` | Rust agent daemon |
| `control-plane/` | Rust control plane server |
| `cli/` | Rust CLI (nixfleet host add, deploy, etc.) |
| `shared/` | Rust shared types |
| `.claude/agents/` | 7 framework agents |
| `.claude/skills/` | 7 framework skills |
| `.claude/rules/` | 3 framework rules |

## Build & Test

```sh
nix fmt                            # format (alejandra + shfmt)
nix flake check --no-build         # eval tests
cargo test --workspace             # Rust tests
nix run .#validate                 # full validation
```

## Testing

3-tier pyramid: **Eval** (instant), **VM** (slow), **Smoke** (real hardware).

## Git Workflow

Branches: `feat/`, `fix/`, `refactor/`, `docs/`, `infra/` prefixes. PRs required for `main`.
