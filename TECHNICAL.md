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
- `_config/` -- raw config files (zsh, kitty, starship, etc.)
- `_hardware/` -- per-host disk and hardware configs
- `core/_home/` -- HM module fragments (imported by `core/home.nix`, not import-tree)

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

### Scopes vs Core

**Core** (`core/`) -- always active on every host. Boot, networking, user, security, secrets, shell tools.

**Scopes** (`scopes/`) -- conditionally active based on hostSpec flags. Each scope owns its config AND its impermanence persist paths.

## The Wrapper / HM Boundary

**This is the most important design decision.** Getting it wrong causes duplication, broken theming, and binary conflicts.

### Rule: HM for local tools, Wrappers for portable composites only

| What | Managed by | Why |
|------|-----------|-----|
| Individual tools (kitty, git, starship, bat, helix, btop, zellij) | HM `programs.*` | Catppuccin auto-themes them, shell integrations work, no conflicts |
| Portable dev shell (`nix run .#shell`) | Wrapper | Self-contained zsh + tools for remote machines |
| Portable terminal (`nix run .#terminal`) | Wrapper | Kitty wrapping the portable shell |
| Desktop session (niri + noctalia) | Wrapper + NixOS module | Portable compositor config |

### Why NOT wrap individual tools

Attempted and reverted. Problems:
1. **Catppuccin breakage** -- catppuccin/nix themes HM `programs.*`. Wrapped kitty doesn't get themed.
2. **Binary conflicts** -- wrapped zsh + HM `programs.zsh.enable` = two zsh binaries in PATH.
3. **Duplication** -- git config in both wrapper (flags) and HM (gitconfig). Drift happens.
4. **Shell integration loss** -- HM `programs.zoxide.enable` adds `eval "$(zoxide init zsh)"` to zshrc. Wrapped zoxide doesn't.

### Config Sharing (`_config/`)

Both HM and wrappers read from `_config/`:

| File | HM consumer | Wrapper consumer |
|------|-----------|-----------------|
| `_config/kitty.conf` | `programs.kitty.extraConfig = readFile` | `terminal.nix` `--config` flag |
| `_config/starship.toml` | `programs.starship.settings = importTOML` | `shell.nix` `STARSHIP_CONFIG` env |
| `_config/gitconfig` | N/A (HM has its own settings + user/signing) | `shell.nix` `GIT_CONFIG_GLOBAL` env |
| `_config/zsh/aliases.zsh` | `initContent` sources it | Bundled in shell zshrc |
| `_config/zsh/functions.zsh` | `initContent` sources it | Bundled in shell zshrc |
| `_config/zsh/wrapperrc.zsh` | N/A | Core zshrc for wrapper |

**Sync requirement:** When changing `_config/zsh/wrapperrc.zsh`, verify `core/_home/zsh.nix` has matching settings (and vice versa). They express the same config in different formats (raw zsh vs Nix options).

## Scope-Aware Impermanence

Persist paths are co-located with their scope, NOT centralized:

| Module | Persist paths |
|--------|---------------|
| `impermanence.nix` | System dirs, user data, .ssh, .gnupg, .config/gh |
| `core/nixos.nix` | Neovim plugins, tmux resurrect, zoxide db |
| `graphical/nixos.nix` | Chrome, Firefox, Brave, VS Code, Slack, halloy |
| `dev/nixos.nix` | Docker, PostgreSQL, npm, cargo, pip, mise, direnv, .claude |
| `desktop/gnome.nix` | dconf, gnome online accounts |
| Host-specific (krach) | JetBrains config/data/cache |

**Rule:** When adding a program to a scope, add its persist paths in the same module with `lib.mkIf hS.isImpermanent`.

## Nix Gotchas

### constructFiles keys
In nix-wrapper-modules, `constructFiles` keys become bash variable names. Use underscores only: `kitty_config`, NOT `kitty.conf` or `kitty-config`.

### perSystem and unfree
`perSystem` pkgs don't inherit `nixpkgs.config.allowUnfree` from NixOS. Unfree packages (claude-code, ruby-mine) must go in NixOS/HM modules, not wrappers.

### catppuccin/nix and Darwin
catppuccin only provides `nixosModules` and `homeModules` -- no `darwinModules`. Importing `nixosModules` into Darwin causes a class mismatch error. Darwin gets catppuccin via the HM module only.

### nix-index-database
Upstream renamed `hmModules` to `homeModules`. Use `inputs.nix-index-database.homeModules.nix-index`.

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
| `impermanence` | Ephemeral root filesystem |
| `agenix` | Age-encrypted secrets |
| `secrets` | Private repo with encrypted secrets |
| `nix-homebrew` + `homebrew-*` | Homebrew on macOS |
| `nixos-anywhere` | Remote NixOS installation via SSH |
| `wrapper-modules` | Portable wrapped composites |
| `nixos-hardware` | Hardware-specific optimizations |
| `nix-index-database` | Pre-built nix-index + comma |
| `treefmt-nix` | Multi-language formatting |
| `catppuccin` | Consistent theming (macchiato + lavender) |
