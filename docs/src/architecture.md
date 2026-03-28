# Architecture

## Purpose

NixFleet is a framework for declarative NixOS fleet management. It provides the library (`mkFleet`, `mkOrg`, `mkHost`, etc.), core NixOS/Darwin modules, scope infrastructure, Rust agent/control-plane, and a framework test fleet. Opinionated modules (scopes, wrappers, HM programs, config files) belong in consuming fleet repos. This separation keeps the framework generic.

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
| `_hardware/` | Per-host disk-config, hardware-configuration | `fleet.nix` host entries |

Fleet repos typically add `_config/` (tool configs) and `core/_home/` (HM fragments) — these are outside the framework.

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

### Framework test fleet

`modules/fleet.nix` contains a minimal test fleet for the framework's own CI. These hosts exist to make eval tests and VM tests pass — they are not a real org fleet:

- 2 individual hosts: `krach` (isImpermanent), `ohm` (userName override)
- 2 VM hosts: `krach-qemu` (isImpermanent), `qemu` (isMinimal)
- 1 server host: `lab` (isServer)
- 3 batch hosts: `edge-01`, `edge-02`, `edge-03` (simulated edge fleet via `mkBatchHosts`)
- 3 test matrix hosts: `test-workstation-x86_64`, `test-server-x86_64`, `test-minimal-x86_64` (via `mkTestMatrix`)

Consuming fleet repos define their own organizations and hosts. See the [fleet repo](https://github.com/abstracts33d/fleet) for a reference implementation.

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
| agenix | Age-encrypted secrets (framework-agnostic, wired via hostSpec) |
| flake-parts | Module system at flake level |
| import-tree | Auto-import all `.nix` files under `modules/` |
| nixos-anywhere | Remote NixOS installation (used by `install` app) |
| nixos-hardware | Hardware configuration modules |
| lanzaboote | Secure Boot support |
| treefmt-nix | Multi-language formatting (alejandra + shfmt) |

## Dependencies

- All modules depend on `module-options.nix` (defines the deferred module option types)
- Host files depend on `_shared/mk-host.nix` and `_shared/host-spec-module.nix`
- `core/nixos.nix` and `core/darwin.nix` depend on `hostSpec` options from `host-spec-module.nix`

## Links

- [Host System](hosts/README.md)
- [Scope System](scopes/README.md)
- [Core Modules](core/README.md)
