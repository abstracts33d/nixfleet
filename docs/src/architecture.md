# Architecture

## Purpose

This repository is a multi-platform Nix configuration targeting NixOS (x86_64, aarch64), macOS (aarch64-darwin, x86_64-darwin), and portable environments. It uses a dendritic architecture where modules self-compose based on host flags.

## Location

- `flake.nix` -- entry point
- `modules/` -- all configuration lives here

## Flake Foundation

The flake is built on two key integrations:

- **flake-parts** -- NixOS module system at the flake level. `flake.nix` calls `inputs.flake-parts.lib.mkFlake` which provides `perSystem`, `flake.modules`, and other structured options.
- **import-tree** -- auto-imports every `.nix` file under `modules/` as a flake-parts module. No manual import lists needed.

```nix
outputs = inputs:
  inputs.flake-parts.lib.mkFlake {inherit inputs;} (
    (inputs.import-tree ./modules) // {
      systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin"];
    }
  );
```

## The `_` Prefix Convention

Files and directories prefixed with `_` are excluded from import-tree auto-import. They are pulled in via explicit `imports` or relative paths:

| Directory | Contains | Imported by |
|-----------|----------|-------------|
| `_shared/` | `mk-host.nix`, `host-spec-module.nix`, `lib/`, disk templates | `fleet.nix`, host constructors |
| `_shared/lib/` | `mk-fleet.nix`, `mk-host.nix`, `mk-org.nix`, `mk-role.nix`, `mk-batch-hosts.nix`, `mk-test-matrix.nix` | `fleet.nix` |
| `_config/` | kitty.conf, starship.toml, zsh/, gitconfig | HM modules + wrappers |
| `_hardware/` | Per-host disk-config, hardware-configuration | `fleet.nix` host entries |
| `core/_home/` | HM tool config fragments | `core/home.nix` |

## Deferred Module Pattern

`module-options.nix` declares three option trees:

```nix
flake.modules.nixos = {};      # NixOS deferred modules
flake.modules.darwin = {};      # Darwin deferred modules
flake.modules.homeManager = {}; # HM deferred modules
```

Modules under `core/` and `scopes/` register into these trees. Host constructors (`mkNixosHost`, `mkDarwinHost`, `mkVmHost`) collect all registered modules via `builtins.attrValues` -- hosts never list features manually. These constructors are internal; the primary public API is `mkFleet`.

## Fleet Composition

All hosts are declared in a single file, `modules/fleet.nix`, using the `mkFleet` API from `_shared/lib/`:

```nix
mkFleet {
  organizations = [myOrg];
  hosts = [
    (mkHost { hostName = "my-host"; org = myOrg; platform = "x86_64-linux"; ... })
    ...
  ] ++ (mkBatchHosts { ... })    # batch hosts (edge fleet, CI)
    ++ (mkTestMatrix { ... });   # role × platform test matrix
}
```

`mkFleet` calls the appropriate constructor (`mkNixosHost`, `mkVmHost`, or `mkDarwinHost`) internally based on each host's `platform` and `isVm` flag. The low-level constructors are no longer part of the host-authoring surface.

### mkFleet building blocks

| Function | Purpose |
|----------|---------|
| `mkOrg` | Declare an organization with shared `hostSpecDefaults` |
| `mkHost` | Declare a single host, associated with an org |
| `mkRole` | Define a named role with default flags and extra modules |
| `mkBatchHosts` | Stamp out N hosts from a template (e.g. edge fleet) |
| `mkTestMatrix` | Generate role × platform eval/CI hosts |

### Internal constructors (called by mkFleet)

| Constructor | Platform | What it adds |
|-------------|----------|-------------|
| `mkNixosHost` | NixOS | Base NixOS + all deferred modules + HM |
| `mkVmHost` | NixOS VM | Wraps `mkNixosHost` + virtio, SPICE, software rendering, global DHCP |
| `mkDarwinHost` | macOS | nix-darwin + all deferred modules + HM |

### Current fleet

The `abstracts33d` organization in `fleet.nix` contains:
- 4 physical hosts: `krach`, `ohm`, `lab` (NixOS), `aether` (Darwin)
- 4 VMs: `krach-qemu`, `qemu` (x86_64), `krach-utm`, `utm` (aarch64)
- 3 batch hosts: `edge-01`, `edge-02`, `edge-03` (simulated edge fleet)
- 3 test matrix hosts: `workstation`, `server`, `minimal` roles on x86_64-linux

## Scope Self-Activation

Scope modules use `lib.mkIf hS.<flag>` to self-activate. Adding a new scope file automatically applies to all hosts with the matching flag. No wiring needed.

## Key Integrations

| Input | Purpose |
|-------|---------|
| nixpkgs (unstable) | Package set |
| home-manager | User environment |
| nix-darwin | macOS system config |
| disko | Declarative disk partitioning |
| impermanence | Ephemeral root filesystem |
| agenix | Age-encrypted secrets |
| catppuccin/nix | Theming (200+ apps) |
| nix-wrapper-modules | Portable composites |
| nixos-anywhere | Remote NixOS installation |
| nix-index-database | command-not-found |
| treefmt-nix | Multi-language formatting |

## Dependencies

- All modules depend on `module-options.nix` (defines the deferred module option types)
- Host files depend on `_shared/mk-host.nix` and `_shared/host-spec-module.nix`
- Wrappers depend on `_config/` shared config files
- HM modules in `core/_home/` depend on `_config/` shared config files

## Links

- [Host System](hosts/README.md)
- [Scope System](scopes/README.md)
- [Core Modules](core/README.md)
