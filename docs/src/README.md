# NixFleet Technical Documentation

Technical reference for the NixFleet fleet management framework. For the API reference, see [mkFleet API](../nixfleet/specs/mk-fleet-api.md). For the user guide, see [NixFleet Guide](../guide/README.md).

## Framework API & Architecture

- [Architecture](architecture.md) — Flake-parts, import-tree, deferred modules, fleet composition
- [mkFleet API Reference](../nixfleet/specs/mk-fleet-api.md) — mkFleet, mkOrg, mkRole, mkHost, mkBatchHosts, mkTestMatrix

## Reference Fleet

The `abstracts33d` organization — 14 hosts defined in `modules/fleet.nix` via `mkFleet`.

- [Host System](hosts/README.md)
  - [krach](hosts/krach.md) -- Main workstation (Niri + greetd, impermanent)
  - [ohm](hosts/ohm.md) -- Secondary laptop (GNOME + GDM, impermanent)
  - [lab](hosts/lab.md) -- Headless server (impermanent)
  - [aether](hosts/aether.md) -- Apple Silicon Mac (nix-darwin)
  - Virtual Machines
    - [VM Overview](hosts/vm/README.md)
    - [krach-qemu](hosts/vm/krach-qemu.md) -- QEMU mirror of krach
    - [krach-utm](hosts/vm/krach-utm.md) -- UTM mirror of krach (aarch64)
    - [qemu](hosts/vm/qemu.md) -- Minimal QEMU test VM
    - [utm](hosts/vm/utm.md) -- Minimal UTM test VM (aarch64)
  - Batch: `edge-01`, `edge-02`, `edge-03` -- simulated edge fleet via `mkBatchHosts`
  - Test matrix: `test-workstation-x86_64`, `test-server-x86_64`, `test-minimal-x86_64` -- role x platform CI via `mkTestMatrix`

## Scopes

- [Scope System](scopes/README.md)
  - [base](scopes/base.md) -- Universal packages (CLI tools, file utils, nix management)
  - [catppuccin](scopes/catppuccin.md) -- Macchiato + lavender theming
  - [nix-index](scopes/nix-index.md) -- command-not-found + comma
  - [impermanence](scopes/impermanence.md) -- Ephemeral root + btrfs wipe
  - [graphical](scopes/graphical.md) -- Pipewire, fonts, browsers, editors
  - [dev](scopes/dev.md) -- Dev tools, Docker, Claude Code, direnv, mise
  - Desktop Environments
    - [niri](scopes/desktop/niri.md) -- Scrollable-tiling Wayland compositor + Noctalia
    - [hyprland](scopes/desktop/hyprland.md) -- Hyprland WM + waybar, wofi, hyprlock
    - [gnome](scopes/desktop/gnome.md) -- GNOME desktop (trimmed bloat)
  - Display Managers
    - [greetd](scopes/display/greetd.md) -- TUI greeter (tuigreet)
    - [gdm](scopes/display/gdm.md) -- GNOME Display Manager
  - Hardware
    - [bluetooth](scopes/hardware/bluetooth.md) -- Bluetooth + Blueman
    - [secure-boot](scopes/hardware/secure-boot.md) -- Lanzaboote Secure Boot
  - Darwin
    - [homebrew](scopes/darwin/homebrew.md) -- Homebrew casks and brews
    - [karabiner](scopes/darwin/karabiner.md) -- Key remapping
    - [aerospace](scopes/darwin/aerospace.md) -- AeroSpace window manager

## Core Modules

- [Core Overview](core/README.md)
  - [nixos](core/nixos.md) -- Boot, networking, secrets, SSH, firewall, users
  - [darwin](core/darwin.md) -- Nix settings, TouchID sudo, dock, system defaults
  - [home](core/home.md) -- HM tool configs (zsh, git, starship, ssh, neovim, tmux)

## Portable Wrappers

- [Wrappers Overview](wrappers/README.md)
  - [shell](wrappers/shell.md) -- Portable zsh + 20 CLI tools
  - [terminal](wrappers/terminal.md) -- Kitty wrapping the portable shell

## Apps

- [Apps Overview](apps/README.md)
  - [install](apps/install.md) -- macOS local + NixOS remote install
  - [build-switch](apps/build-switch.md) -- Day-to-day rebuild
  - [validate](apps/validate.md) -- Full validation suite
  - [spawn-qemu](apps/spawn-qemu.md) -- QEMU VM launcher (Linux)
  - [spawn-utm](apps/spawn-utm.md) -- UTM VM guide (macOS)
  - [test-vm](apps/test-vm.md) -- Automated ISO-to-verify cycle

## Testing

- [Test Pyramid](testing/README.md)
  - [Eval Tests](testing/eval-tests.md) -- Tier C: config correctness
  - [VM Tests](testing/vm-tests.md) -- Tier A: runtime assertions

## Claude Code Integration

- [Claude Overview](claude/README.md)
  - [Scopes](claude/scopes.md) -- 3-scope instruction system
  - [Permissions](claude/permissions.md) -- 3-level security model
  - [Agents](claude/agents.md) -- 7 specialized agents
  - [Skills](claude/skills.md) -- 10 orchestration skills
  - [Hooks](claude/hooks.md) -- 7 automation hooks
  - [MCP](claude/mcp.md) -- MCP server integrations
  - [Rules](claude/rules.md) -- 8 project rules

## Secrets Management

- [Secrets Overview](secrets/README.md)
  - [Bootstrap](secrets/bootstrap.md) -- Install key provisioning
  - [WiFi](secrets/wifi.md) -- WiFi network bootstrap
