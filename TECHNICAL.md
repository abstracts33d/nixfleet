# Technical Architecture

Deep-dive into NixFleet's design decisions, framework internals, and Nix gotchas.

For a high-level overview, see [ARCHITECTURE.md](ARCHITECTURE.md). For getting started, see [QUICKSTART.md](QUICKSTART.md). For full docs, see [docs/src/](docs/src/).

## NixFleet Framework

### Composition Pipeline

```
fleet.nix
  └── mkFleet { organizations, hosts }
        ├── mkOrg { name, defaults }
        ├── mkRole { flags, modules }        # workstation, server, edge, minimal, ...
        └── mkHost { hostName, org, role }
              └── constructors (mkNixosHost / mkDarwinHost)
                    └── nixpkgs.lib.nixosSystem / darwin.lib.darwinSystem
```

**Composition order:** org defaults < role defaults < host values. Each layer can override the previous using `lib.mkDefault` (org/role) vs plain values (host).

### Framework Distribution

The framework API is exported for external consumers via two mechanisms:

1. **flake-parts:** `flakeModules.default` — external flakes add `imports = [inputs.nixfleet.flakeModules.default]` and get `config.nixfleet.lib.mkFleet` etc.
2. **Plain lib:** `lib.nixfleet.mkFleet` — for non-flake-parts consumers.

The monorepo wrapper (`modules/flake-module.nix`) ties the import-tree-based internals to these public exports.

### Decontamination Principle

The framework provides **mechanism**, not **policy**:
- `mkFleet`, `mkOrg`, `mkRole`, `mkHost` are generic constructors with no org-specific assumptions
- The `abstracts33d` organization (org defaults, host list, secrets paths) lives in `fleet.nix` as a client of the framework
- Secrets are referenced by path, never by content — the framework is secrets-backend-agnostic (agenix, sops, vault)

### Framework API (`modules/_shared/lib/`)

| Function | Purpose |
|----------|---------|
| `mkFleet` | Top-level: organizations + hosts -> nixosConfigurations + darwinConfigurations |
| `mkOrg` | Organization with shared defaults |
| `mkRole` | Composable role (sets hostSpec flags) |
| `mkHost` | Single host definition |
| `mkBatchHosts` | N identical hosts from a template |
| `mkTestMatrix` | Cross-product of roles x platforms for CI |

## Core Concepts

### Dendritic Pattern (flake-parts + import-tree)

Every `.nix` file under `modules/` is automatically imported as a flake-parts module. No manual imports list — drop a file in the right directory and it's active.

```
flake.nix -> mkFlake -> import-tree ./modules -> every .nix file is a flake-parts module
```

The `_` prefix convention excludes directories from auto-import:
- `_shared/` -- helper functions, option definitions, framework API
- `_hardware/` -- per-host disk and hardware configs

### Deferred Modules + Auto-Inclusion

Modules register deferred NixOS/Darwin/HM modules via `flake.modules.{nixos,darwin,homeManager}.*`. The `mkNixosHost`/`mkDarwinHost` constructors auto-include ALL registered modules using `builtins.attrValues`:

```nix
# In mk-host.nix:
modules = hardwareModules
  ++ [hostSpecModule {hostSpec = hostSpecValues;}]
  ++ (builtins.attrValues nixosModules)   # auto-include everything
  ++ extraNixosModules;
```

Each scope module self-gates with `lib.mkIf hS.<flag>`. Adding a new scope file = automatic for all hosts. No manual feature lists.

### Framework vs Fleet

**Framework** (`core/`) -- always active on every host. Boot, networking, user, security, shell basics. Provides mechanism, not policy.

**Fleet** (your repo) -- opinionated modules: scopes (graphical, dev, desktop, display, hardware, darwin), wrappers (shell, terminal), HM programs (zsh, git, starship, nvim, etc.), config files, theming. These register as deferred modules that the framework auto-includes.

### Scope Pattern (fleet-side)

Scope modules live in your fleet and self-gate with `lib.mkIf hS.<flag>`:
- `isGraphical` -- pipewire, fonts, browsers
- `isDev` -- direnv, docker, claude-code
- `useNiri` -- niri compositor + greetd
- `isImpermanent` -- ephemeral root, persist paths
- etc.

Persist paths should be co-located with their scope, NOT centralized. When adding a program to a scope, add its persist paths in the same module with `lib.mkIf hS.isImpermanent`.

## Nix Gotchas

### perSystem and unfree
`perSystem` pkgs don't inherit `nixpkgs.config.allowUnfree` from NixOS. Unfree packages must go in NixOS/HM modules, not perSystem apps.

### Backup file collisions
`backupFileExtension` creates fixed-name backups that block future activations. Use `backupCommand` with timestamped names and pruning:
```nix
backupCommand = ''mv {} {}.nbkp.$(date +%Y%m%d%H%M%S) && ls -t {}.nbkp.* 2>/dev/null | tail -n +6 | xargs -r rm -f'';
```

### home.file force
SSH public keys and other managed files that should always be overwritten: use `force = true`.

### networking.interfaces guard
`networking.interfaces."${name}".useDHCP` crashes if name is empty. Guard with `lib.mkIf (hS.networking ? interface)`.

### networking.useDHCP priority
Don't use `mkDefault` for `networking.useDHCP = false` in core -- it conflicts with `hardware-configuration.nix`'s `mkDefault true` (same priority). Use plain value.

## Flake Inputs

| Input | Purpose |
|-------|---------|
| `nixpkgs` | Package repository (nixos-unstable) |
| `darwin` | nix-darwin macOS system config |
| `home-manager` | User environment management |
| `flake-parts` | Module system for flake outputs |
| `import-tree` | Auto-import directory tree as modules |
| `disko` | Declarative disk partitioning |
| `impermanence` | Ephemeral root filesystem (fleet-consumed) |
| `agenix` | Age-encrypted secrets |
| `nixos-anywhere` | Remote NixOS installation via SSH |
| `nixos-hardware` | Hardware-specific optimizations |
| `lanzaboote` | Secure Boot (fleet-consumed) |
| `treefmt-nix` | Multi-language formatting |

Opinionated inputs (catppuccin, nix-index-database, wrapper-modules, nix-homebrew) are added by fleets that need them.
