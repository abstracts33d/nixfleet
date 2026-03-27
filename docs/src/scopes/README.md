# Scope System

## Purpose

Scopes are feature modules that self-activate based on `hostSpec` flags. Each scope registers deferred modules via `config.flake.modules.{nixos,darwin,homeManager}.<name>` and gates its config with `lib.mkIf hS.<flag>`. Adding a new scope file automatically applies to all hosts with the matching flag.

## Location

- `modules/scopes/` -- all scope modules
- `modules/_shared/host-spec-module.nix` -- flag definitions

## Framework Scopes

The framework ships a small set of scopes. Consuming fleet repos add their own opinionated scopes (graphical, dev, desktop, etc.) on top.

| Scope | Flag | Description |
|-------|------|-------------|
| [base](base.md) | `!isMinimal` | Universal CLI packages (NixOS + Darwin + HM) |
| [impermanence](impermanence.md) | `isImpermanent` | Btrfs root wipe + system/user persistence paths |
| [nixfleet-agent](nixfleet-agent.md) | `services.nixfleet-agent.enable` | Fleet management agent systemd service |
| [nixfleet-control-plane](nixfleet-control-plane.md) | `services.nixfleet-control-plane.enable` | Control plane HTTP server |

> **Note:** Scopes like `graphical`, `dev`, `niri`, `gnome`, `catppuccin`, `darwin/*`, and enterprise scopes are not part of the framework. They are defined in consuming fleet repos. The `hostSpec` options for those flags are also declared by the consuming fleet.

## Scope Self-Activation Pattern

Scope modules register into the deferred module trees and gate with `lib.mkIf`:

```nix
# modules/scopes/example.nix
{...}: {
  flake.modules.nixos.example = { config, lib, ... }: let
    hS = config.hostSpec;
  in {
    config = lib.mkIf hS.someFlag {
      # ... configuration
    };
  };
}
```

## Built-in Roles

Roles (`modules/_shared/lib/roles.nix`) bundle `hostSpec` flags into named presets. When a host uses a role, the role's flags determine which scopes activate. Six built-in roles:

| Role | Flags set | Notes |
|------|-----------|-------|
| `workstation` | `isDev`, `isGraphical`, `isImpermanent` | Expects fleet to define dev/graphical/desktop scopes |
| `server` | `isServer` | Headless, no graphical or dev tooling |
| `minimal` | `isMinimal` | Bare minimum — no base packages |
| `vm-test` | `isGraphical`, `isImpermanent` | For VM test nodes |
| `edge` | `isServer`, `isMinimal` | Minimal edge device |
| `darwin-workstation` | `isDarwin`, `isDev`, `isGraphical` | macOS with dev tools |

Roles are assigned via `mkHost` or `mkBatchHosts` in `fleet.nix`. Individual `hostSpecValues` can override any role default.

## Persist Paths Pattern

Impermanence persist paths live alongside their program definitions, not in a central file. Each scope adds its own `home.persistence."/persist".directories` when `isImpermanent` is true.

## Adding a New Scope

1. Create `modules/scopes/<scope>.nix`
2. Register deferred modules gated by a `hostSpec` flag
3. Add the flag to `host-spec-module.nix` if new (or extend it in your fleet)
4. All matching hosts automatically get the scope — no manual wiring

## Links

- [Architecture](../architecture.md)
- [Host System](../hosts/README.md)
