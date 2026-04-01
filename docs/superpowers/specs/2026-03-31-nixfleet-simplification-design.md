# Nixfleet Simplification: From DSL to Standard NixOS

**Date:** 2026-03-31
**Status:** Draft
**Scope:** nixfleet (framework) + fleet (reference fleet)

## Problem

Nixfleet's Nix layer has accumulated abstractions (mkFleet, mkOrg, mkRole, mkHost, deferred module registration, ~500 lines of custom shell scripts in apps.nix) that make standard NixOS operations harder than they should be. Deploying a single machine requires understanding the framework instead of just running `nixos-anywhere`.

## Goals

1. **nixfleet is the product** — a framework that provides useful NixOS/Darwin modules, a hostSpec system, and fleet management (Rust agent/control-plane/CLI)
2. **Orgs define fleet repos** — standard Nix flakes that consume nixfleet, no framework ceremony
3. **Single-machine install is one command** — `nixos-anywhere --flake .#web-01 root@192.168.1.50`

## Non-goals

- Automated fleet management via Rust agent/CP (future work — architecture must not block it)
- Convention-over-configuration auto-discovery (Approach C — future layer on top)
- Presets or role system (flags are sufficient)

## Design

### mkHost: The Single API

`nixfleet.lib.mkHost` is the only function framework consumers need. It takes a host definition and returns a `nixosSystem` or `darwinSystem`.

```nix
nixfleet.lib.mkHost {
  hostName = "web-01";
  platform = "x86_64-linux";  # or "aarch64-linux", "aarch64-darwin", "x86_64-darwin"
  stateVersion = "24.11";
  hostSpec = {
    userName = "s33d";
    timeZone = "Europe/Paris";
    isGraphical = true;
    isImpermanent = true;
    isDev = true;
  };
  modules = [
    ./hosts/web-01/hardware-configuration.nix
    ./hosts/web-01/disk-config.nix
  ];
}
```

**Parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `hostName` | string | yes | Machine hostname |
| `platform` | string | yes | One of: `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`, `x86_64-darwin` |
| `stateVersion` | string | yes | NixOS/Darwin state version |
| `hostSpec` | attrset | yes | Host configuration flags (extensible by fleet modules) |
| `modules` | list | no | Additional NixOS/Darwin modules |

**What mkHost does internally:**

`mkHost` is a closure over nixfleet's pinned inputs (nixpkgs, home-manager, disko, impermanence, agenix, etc.). Fleet repos don't need to pass these — they're baked into `nixfleet.lib.mkHost` at flake evaluation time.

1. Detects Darwin vs NixOS from `platform`
2. Calls `nixpkgs.lib.nixosSystem` or `darwin.lib.darwinSystem` (using nixfleet's pinned nixpkgs)
3. Injects nixfleet core modules (nix settings, boot, SSH hardening, networking)
4. Sets `config.hostSpec` from the provided `hostSpec` attrset (all via `lib.mkDefault`)
5. Imports all nixfleet scopes (they self-activate based on hostSpec flags via `lib.mkIf`)
6. Appends user-provided `modules`

**What mkHost does NOT do:**

- No org concept — that's a `let` binding in the fleet's flake.nix
- No role concept — just hostSpec flags
- No fleet-level validation or orchestration

### hostSpec: Extensible by Fleet Repos

nixfleet defines base hostSpec options:

- `userName`, `hostName`, `timeZone`, `locale`, `keyboardLayout`
- `isGraphical`, `isImpermanent`, `isDev`, `isMinimal`
- `sshAuthorizedKeys`

Fleet repos declare additional hostSpec options via NixOS modules (the existing `host-spec-fleet.nix` pattern):

```nix
# fleet module that extends hostSpec
{ lib, ... }: {
  options.hostSpec = {
    useHyprland = lib.mkOption { type = lib.types.bool; default = false; };
    hasBluetooth = lib.mkOption { type = lib.types.bool; default = false; };
    cpuVendor = lib.mkOption { type = lib.types.nullOr (lib.types.enum ["amd" "intel"]); default = null; };
    theme.flavor = lib.mkOption { type = lib.types.str; default = "macchiato"; };
    theme.accent = lib.mkOption { type = lib.types.str; default = "peach"; };
  };
}
```

mkHost accepts arbitrary hostSpec keys — the NixOS module system validates them.

### Fleet Repo Structure

A fleet repo is a standard Nix flake:

```
fleet/
├── flake.nix
├── flake.lock
├── hosts/
│   ├── web-01/
│   │   ├── default.nix
│   │   ├── disk-config.nix
│   │   └── hardware-configuration.nix
│   ├── srv-01/
│   │   └── ...
│   └── mac-01/
│       └── ...
├── modules/
│   ├── host-spec-fleet.nix    # fleet-specific hostSpec options
│   ├── scopes/                # fleet-level scopes (catppuccin, hyprland, dev, etc.)
│   └── core/                  # fleet-level HM config (git, zsh, etc.)
└── secrets/                   # or as a separate flake input
```

**Example flake.nix:**

```nix
{
  inputs = {
    nixfleet.url = "github:abstracts33d/nixfleet";
    nixpkgs.follows = "nixfleet/nixpkgs";
    home-manager.follows = "nixfleet/home-manager";
    secrets.url = "git+ssh://git@github.com/abstracts33d/fleet-secrets";
    # fleet-specific inputs
    catppuccin.url = "github:catppuccin/nix";
  };

  outputs = { nixfleet, secrets, catppuccin, ... } @ inputs: let
    mkHost = nixfleet.lib.mkHost;

    org = {
      userName = "s33d";
      timeZone = "Europe/Paris";
      locale = "en_US.UTF-8";
    };

    fleetModules = [
      ./modules/host-spec-fleet.nix
      ./modules/core/home.nix
      ./modules/scopes
      # secrets wiring, catppuccin, etc.
    ];
  in {
    nixosConfigurations.web-01 = mkHost {
      hostName = "web-01";
      platform = "x86_64-linux";
      stateVersion = "24.11";
      hostSpec = org // {
        isGraphical = true;
        isImpermanent = true;
        isDev = true;
        useHyprland = true;
      };
      modules = fleetModules ++ [
        ./hosts/web-01/hardware-configuration.nix
        ./hosts/web-01/disk-config.nix
      ];
    };

    nixosConfigurations.srv-01 = mkHost {
      hostName = "srv-01";
      platform = "x86_64-linux";
      stateVersion = "24.11";
      hostSpec = org // { isImpermanent = true; };
      modules = fleetModules ++ [
        ./hosts/srv-01/hardware-configuration.nix
        ./hosts/srv-01/disk-config.nix
      ];
    };

    darwinConfigurations.mac-01 = mkHost {
      hostName = "mac-01";
      platform = "aarch64-darwin";
      stateVersion = "24.11";
      hostSpec = org // { isDev = true; };
      modules = fleetModules ++ [ ./hosts/mac-01/default.nix ];
    };
  };
}
```

### Deployment Commands

All deployment uses standard NixOS tooling. No custom scripts.

**Initial install (fresh machine):**

```bash
nixos-anywhere --flake .#web-01 root@192.168.1.50
```

nixos-anywhere handles: boot into kexec, run disko (format disks from the nixosConfiguration's disko config), install NixOS, reboot.

**Rebuild (existing machine):**

```bash
# Local
sudo nixos-rebuild switch --flake .#web-01

# Remote
nixos-rebuild switch --flake .#web-01 --target-host root@192.168.1.50
```

**macOS:**

```bash
darwin-rebuild switch --flake .#mac-01
```

**Custom ISO (for fresh installs):**

nixfleet still provides a custom minimal NixOS installer ISO with pre-baked SSH keys for passwordless access. Boot the target machine from this ISO, then run `nixos-anywhere` from the fleet repo.

```bash
# Build the ISO (from nixfleet directly — fleet repos don't need to re-export it)
nix build github:abstracts33d/nixfleet#packages.x86_64-linux.iso
# Or if nixfleet is a local checkout:
nix build /path/to/nixfleet#packages.x86_64-linux.iso

# Boot target from ISO, then:
nixos-anywhere --flake .#web-01 root@<target-ip>
```

### What Stays in nixfleet

| Component | Notes |
|-----------|-------|
| `mkHost` | Single API function |
| Core modules (`core/nixos.nix`, `core/darwin.nix`) | Nix settings, boot, SSH hardening, networking |
| hostSpec module | Base options, extensible by fleet modules |
| Framework scopes (`base`, `impermanence`) | Self-activate on hostSpec flags |
| Agent/CP service modules (`services.nixfleet-agent`, `services.nixfleet-control-plane`) | The fleet management product — standard NixOS service options |
| Disko templates (`btrfs-disk.nix`, `btrfs-impermanence-disk.nix`) | Reusable by fleet repos |
| ISO module | Custom installer with SSH keys |
| VM helpers (`spawn-qemu`, `launch-vm`, `test-vm`, `spawn-utm`) | Exported as `nixfleet.lib.mkVmApps` — fleet repos wire them into their own `apps` output (see VM Helpers section) |
| Eval tests | Flake checks |
| VM tests | nixosTest suites |
| Rust crates (agent, control-plane, CLI, shared types) | The product |
| Formatter (treefmt: alejandra + shfmt) | |

### What Gets Removed from nixfleet

| Component | Reason |
|-----------|--------|
| `mkFleet` | Replaced by standard `nixosConfigurations` in fleet repos |
| `mkOrg` | Replaced by `let` bindings |
| `mkRole` / `roles.nix` | Replaced by hostSpec flags |
| `mkBatchHosts` | Trivial `builtins.map` over mkHost when needed |
| `mkTestMatrix` | Trivial helper when needed |
| `install` app | `nixos-anywhere` directly |
| `build-switch` app | `nixos-rebuild` directly |
| `docs` app | Not a framework concern |
| Deferred module registration pattern | Scopes become plain NixOS modules imported by mkHost directly |
| `flake-module.nix` (flakeModules export) | Replaced by simpler exports |

### nixfleet Exports

Current (flake-parts flakeModules):

```nix
inputs.nixfleet.flakeModules.default
inputs.nixfleet.flakeModules.apps
inputs.nixfleet.flakeModules.tests
inputs.nixfleet.flakeModules.iso
```

New (plain flake outputs):

```nix
nixfleet.lib.mkHost                              # the API
nixfleet.nixosModules.core                       # for users who want modules without mkHost
nixfleet.templates.default                       # nix flake init -t nixfleet (future, Approach C)
nixfleet.packages.${system}.iso                  # custom installer ISO
nixfleet.packages.${system}.nixfleet-agent       # Rust agent binary
nixfleet.packages.${system}.nixfleet-cp          # Rust control-plane binary
nixfleet.packages.${system}.nixfleet-cli         # Rust CLI binary
nixfleet.diskoTemplates.btrfs                    # standard btrfs disk template
nixfleet.diskoTemplates.btrfs-impermanence       # btrfs with impermanence layout
```

nixfleet stays flake-parts internally for its own organization (multi-system packages, formatter, checks). Fleet repos don't need to know or care about this.

### Scope Module Changes

Currently scopes register via the deferred module pattern:

```nix
# old: scope registers itself
config.flake.modules.nixos.impermanence = { ... };
```

New: scopes are plain NixOS modules that mkHost imports from a known directory. They self-activate:

```nix
# new: scope is just a NixOS module
{ config, lib, ... }:
let hS = config.hostSpec;
in {
  config = lib.mkIf hS.isImpermanent {
    # impermanence config
  };
}
```

mkHost collects all scope modules from `modules/scopes/` and includes them. No registration needed.

### VM Helpers

VM helpers (`spawn-qemu`, `launch-vm`, `test-vm`, `spawn-utm`) need fleet context (they build and run the fleet's `nixosConfigurations`). They can't be standalone nixfleet packages.

nixfleet exports a library function that fleet repos wire into their own apps:

```nix
# In fleet's flake.nix outputs:
apps = nixfleet.lib.mkVmApps {
  inherit (self) nixosConfigurations;
  # Returns: { spawn-qemu, launch-vm, test-vm, spawn-utm }
};
```

This keeps the VM helper logic in nixfleet (maintained once) while fleet repos provide the host configs they operate on. Fleet repos opt-in explicitly — no flakeModules magic.

### Agent Configuration

The nixfleet agent is a standard NixOS service module, not a hostSpec flag. mkHost auto-includes it (disabled by default). Fleet repos enable and configure it:

```nix
# nixfleet provides (auto-included by mkHost, inactive until enabled):
{ config, lib, pkgs, ... }: {
  options.services.nixfleet-agent = {
    enable = lib.mkEnableOption "NixFleet agent";
    controlPlaneUrl = lib.mkOption { type = lib.types.str; };
    machineId = lib.mkOption { type = lib.types.str; default = config.networking.hostName; };
    pollInterval = lib.mkOption { type = lib.types.int; default = 60; };
    # TLS cert paths, auth config, etc.
  };

  config = lib.mkIf config.services.nixfleet-agent.enable {
    systemd.services.nixfleet-agent = { /* ... */ };
  };
}
```

Fleet repos configure it per-host or fleet-wide:

```nix
# Fleet-wide (in fleetModules):
{ services.nixfleet-agent.enable = true; services.nixfleet-agent.controlPlaneUrl = "https://cp.example.com"; }

# Per-environment override:
# staging hosts → staging CP, prod hosts → prod CP
```

This follows NixOS conventions (`services.*`), supports secrets via agenix for TLS certs and auth tokens, and gives enterprise customers full override semantics.

### Disko Integration

Disko templates stay in nixfleet:

```nix
# In a fleet host's disk-config.nix
{ inputs, ... }: {
  imports = [
    # Use nixfleet's template
    inputs.nixfleet.diskoTemplates.btrfs-impermanence
  ];
  # Override device
  disko.devices.disk.main.device = "/dev/nvme0n1";
}
```

Or fleet repos can define their own disko configs from scratch — the templates are optional.

Because disko config is part of the nixosConfiguration, `nixos-anywhere` automatically uses it for disk formatting.

### Input Follows Strategy

Fleet repos use `follows` to deduplicate shared inputs:

```nix
nixpkgs.follows = "nixfleet/nixpkgs";
home-manager.follows = "nixfleet/home-manager";
disko.follows = "nixfleet/disko";
```

This is a conscious choice: nixfleet controls the nixpkgs pin, ensuring framework modules are tested against that exact version. Fleet repos inherit it. If nixfleet lags behind, fleet cannot independently update nixpkgs without breaking the `follows` chain. This is acceptable — the framework should be the source of truth for dependency versions.

## Related Specs

- `2026-03-28-nixfleet-gtm-solo-design.md` — Go-to-market strategy. This simplification is a prerequisite for Phase 2 (Open Source).

## Execution Order

```
1. This spec (simplification)         — clean the foundation
2. Phase 1 (Rust hardening)           — make the product production-ready
3. Phase 2 (Open Source)              — ship with clean API + hardened product
4. Phase 3 (Framework Infrastructure) — Attic, microvm modules
5. Phase 4 (Consulting + Enterprise)  — ongoing
```

## Future Work

### Approach C: mkFleetFlake (convention-over-configuration)

A convenience layer on top of mkHost for fleets with many hosts:

```nix
outputs = inputs: nixfleet.lib.mkFleetFlake {
  inherit inputs;
  orgDefaults = { userName = "s33d"; timeZone = "Europe/Paris"; };
  hosts = ./hosts;  # auto-discovered from directory structure
};
```

`mkFleetFlake` internally calls mkHost for each discovered host directory. This is sugar, not a replacement — mkHost remains the primitive.

### Batch Hosts

For edge device fleets or identical machines, a simple helper:

```nix
let
  edgeHosts = builtins.listToAttrs (map (i: {
    name = "edge-${toString i}";
    value = mkHost {
      hostName = "edge-${toString i}";
      platform = "aarch64-linux";
      hostSpec = org // { isMinimal = true; };
      modules = [ ./hosts/edge/common.nix ];
    };
  }) (lib.range 1 50));
in {
  nixosConfigurations = { web-01 = ...; srv-01 = ...; } // edgeHosts;
}
```

No framework function needed — it's standard Nix.

### Fleet Management Automation

The Rust agent/control-plane/CLI operates at the OS level and is unaffected by this simplification. The agent polls the control-plane for desired generation, runs `nixos-rebuild switch`, and reports status. This works regardless of how hosts are defined in Nix.

## Migration Plan

### Phase 1: Rebuild mkHost in nixfleet

1. Implement new `mkHost` in `modules/_shared/lib/` that returns `nixosSystem`/`darwinSystem` directly
2. Remove `mkFleet`, `mkOrg`, `mkRole`, `mkBatchHosts`, `mkTestMatrix`
3. Remove deferred module registration — scopes become plain modules imported by mkHost
4. Remove `install`, `build-switch`, `docs` apps from `apps.nix`
5. Update exports: `nixfleet.lib.mkHost`, `nixfleet.packages`, `nixfleet.nixosModules`
6. Update eval tests to work with new mkHost
7. Update VM tests
8. Update `examples/client-fleet/` to new pattern
9. Update documentation

### Phase 2: Migrate fleet repo

1. Rewrite `flake.nix`: use `nixfleet.lib.mkHost` directly, define hosts as `nixosConfigurations`
2. Extract host definitions from `modules/fleet.nix` into `hosts/<name>/default.nix`
3. Org defaults become a `let` binding
4. `host-spec-fleet.nix` stays as a fleet module
5. Scopes stay as-is
6. Remove flakeModules imports

### Phase 3: Verify

1. `nixos-anywhere --flake .#web-01 root@<ip>` works (fresh install)
2. `sudo nixos-rebuild switch --flake .#web-01` works (local rebuild)
3. `nixos-rebuild switch --flake .#web-01 --target-host root@<ip>` works (remote rebuild)
4. `darwin-rebuild switch --flake .#mac-01` works
5. `nix flake check` passes (eval tests)
6. VM tests pass
7. ISO builds and works for bootstrapping fresh machines
