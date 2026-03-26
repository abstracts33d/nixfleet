# Architecture

High-level overview of NixFleet. For detailed internals, see [TECHNICAL.md](TECHNICAL.md). For full docs, see [docs/src/](docs/src/).

## System Overview

```
+-----------------------------------------+
|  Client Fleet (fleet.nix)               |
|  Organizations -> Roles -> Hosts        |
+-----------------------------------------+
|  NixFleet Framework (lib/)              |
|  mkFleet, mkOrg, mkRole, mkHost        |
+-----------------------------------------+
|  Rust Workspace                         |
|  Agent <-> Control Plane <-> CLI        |
+-----------------------------------------+
|  NixOS Module System                    |
|  Core (base) + Fleet Scopes             |
+-----------------------------------------+
```

## Data Flow

```
fleet.nix (host definitions)
    |
    v
mkFleet (framework API)
    |
    v
nixosConfigurations / darwinConfigurations (Nix outputs)
    |
    v
deploy (nixos-anywhere / build-switch)
    |
    v
nixfleet-agent (on each host, reports to CP)
    |
    v
nixfleet-control-plane (central registry, orchestration)
    ^
    |
nixfleet CLI (operator commands)
```

## Composition Order

Each layer can override the previous:

```
Organization defaults    (lib.mkDefault — lowest priority)
    |
    v
Role defaults            (lib.mkDefault — same priority, merged)
    |
    v
Host values              (plain values — highest priority)
```

Example: an org sets `isDev = true` for all hosts. The `minimal` role overrides it to `false`. A specific host can override it back to `true`.

## Framework vs Client Separation

**Framework** (`modules/_shared/lib/`): Generic constructors with no org-specific assumptions. Exported via `flakeModules.default` for external consumers.

**Client** (your fleet repo): Org defaults, host list, secrets paths, scope implementations, HM programs, wrappers. The framework provides a minimal test fleet in `fleet.nix` for CI.

This separation means an external organization can consume the framework without forking:

```nix
{
  inputs.nixfleet.url = "github:abstracts33d/fleet";

  outputs = { nixfleet, ... }: {
    imports = [ nixfleet.flakeModules.default ];
    # Use nixfleet.lib.nixfleet.mkFleet { ... } with your own fleet
  };
}
```

## Nix Module Layers

### Core (always active, framework-provided)

`modules/core/` -- boot, networking, user accounts, security, zsh, git. Every host gets these regardless of flags.

### Scopes (flag-gated, fleet-provided)

Scope modules live in fleet repos, not in the framework. They self-activate with `lib.mkIf hS.<flag>` and co-locate impermanence persist paths. Common scopes: graphical, dev, desktop, display, hardware, darwin.

### HM Programs, Wrappers, Config Files (fleet-provided)

HM program configs (starship, nvim, tmux, kitty, etc.), portable wrappers (shell, terminal), and raw config files live in fleet repos.

## Rust Workspace

Four crates, one Cargo workspace:

| Crate | Type | Purpose |
|-------|------|---------|
| `agent/` | Binary | State machine on each managed host. Registers, polls for config, deploys, reports status |
| `control-plane/` | Binary | Axum HTTP server. Machine registry, deployment scheduling, health tracking |
| `cli/` | Binary | Operator-facing commands: deploy, status, rollback |
| `shared/` | Library | Common types and API contracts shared across crates |

Each Rust binary is packaged as a Nix derivation (e.g., `agent/default.nix`) and can be included in host configurations.

## Two-Repo Strategy

| Repo | Content |
|------|---------|
| `nixfleet` (this repo) | Framework (lib + core) + test fleet |
| `fleet-secrets` (private) | Age-encrypted secrets (SSH keys, passwords, WiFi) |

Secrets are referenced by path in the public repo. The private repo is a flake input (`inputs.secrets`). Update with `nix flake update secrets`.

## Key Design Decisions

1. **Dendritic import**: Every `.nix` under `modules/` is auto-imported. No import lists to maintain.
2. **Deferred modules**: Scope modules register themselves; constructors auto-include all via `builtins.attrValues`.
3. **Central fleet definition**: All hosts in `fleet.nix`, not scattered across directories.
4. **Framework = lib + base**: Opinionated modules (scopes, wrappers, HM programs) live in fleet repos.
5. **Scope-aware impermanence**: Persist paths live alongside their program definitions in fleet scopes.
6. **Mechanism over policy**: Framework provides constructors; fleets provide opinions.
