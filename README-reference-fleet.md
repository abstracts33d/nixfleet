# NixFleet

**Declarative NixOS fleet management.** Define your organization's infrastructure as code — workstations, servers, edge devices — with reproducible builds, instant rollback, and zero config drift.

## What is NixFleet?

NixFleet is an open-core framework for managing fleets of NixOS machines. It provides:
- **Organizations** — group hosts by org with shared defaults
- **Roles** — compose workstation, server, edge, kiosk profiles from reusable building blocks
- **Batch provisioning** — deploy 3 or 300 identical machines from a template
- **Test matrix** — validate every role x platform combination in CI
- **Extension points** — plug in commercial modules (dashboard, RBAC, SSO)

## Quick Start

```nix
# Your fleet in ~20 lines
let fleet = mkFleet {
  organizations = [ myOrg ];
  hosts = [ ... ];
};
```

See the [User Guide](docs/guide/README.md) for a full walkthrough, or jump to [Adding a New Host](#adding-a-new-host) below.

## Reference Fleet

This repository is the NixFleet reference implementation, dogfooding the framework with the `abstracts33d` organization:
- 4 physical hosts (krach, ohm, lab, aether)
- 4 development VMs
- 3 edge batch hosts
- 3 test matrix hosts

## Layout

```
.
├── apps/              # Shell scripts for build-switch and rollback
├── modules/           # Everything lives here (flake-parts + import-tree)
│   ├── core/          # Core deferred modules (always active on every host)
│   │   ├── nixos.nix  # Universal NixOS: boot, networking, user, security, secrets
│   │   ├── darwin.nix # Universal Darwin: nix settings, user, sudo, dock, system defaults
│   │   ├── home.nix   # Universal HM: imports _home/ tools
│   │   └── _home/     # Per-tool HM configs (zsh, git, starship, ssh, kitty, neovim, tmux, etc.)
│   ├── scopes/        # Scope-based modules (self-activate via mkIf on hostSpec flags)
│   │   ├── catppuccin.nix  # Theming (macchiato + lavender)
│   │   ├── nix-index.nix   # command-not-found + comma
│   │   ├── graphical/ # isGraphical: pipewire, fonts, browsers, editors
│   │   ├── dev/       # isDev: direnv, mise, dev packages
│   │   ├── desktop/   # Desktop environments (niri, hyprland, gnome)
│   │   ├── display/   # Standalone display managers (gdm, greetd)
│   │   ├── hardware/  # Hardware (bluetooth, secure-boot)
│   │   ├── enterprise/ # Enterprise features (vpn, filesharing, auth, printing, certs, proxy)
│   │   ├── darwin/    # Darwin-specific (homebrew, karabiner, aerospace)
│   │   ├── impermanence.nix
│   │   └── base.nix   # Universal packages for all hosts
│   ├── wrappers/      # Portable composites (shell, terminal)
│   ├── fleet.nix      # Central host definitions via mkFleet (all orgs + hosts)
│   ├── _shared/       # Framework API (lib/), hostSpec options, disk templates
│   ├── _config/       # Shared config files (kitty.conf, starship.toml, gitconfig, zsh/)
│   └── _hardware/     # Per-host disk-config and hardware-configuration
├── agent/             # Rust fleet agent (nixfleet-agent binary)
├── cli/               # Rust CLI (nixfleet binary)
├── control-plane/     # Rust control plane (nixfleet-control-plane binary)
├── shared/            # Rust shared types (nixfleet-types library)
├── demo/              # Demo fleet scripts and client-fleet example
├── examples/          # Example configurations
├── docs/              # Design specs and implementation plans
└── iso/               # ISO download instructions (see iso/README.md)
```

## Hosts

All hosts in the reference fleet belong to the `abstracts33d` organization and are defined centrally in `modules/fleet.nix` via `mkFleet`.

| Host | Platform | Profile | Description |
|------|----------|---------|-------------|
| **krach** | x86_64-linux | Graphical, Dev, Niri + greetd | Main workstation |
| **ohm** | x86_64-linux | Graphical, GNOME + GDM | Secondary laptop |
| **lab** | x86_64-linux | Server, headless | Headless server |
| **aether** | aarch64-darwin | macOS | Apple Silicon Mac |
| **krach-qemu** | x86_64-linux | Niri + greetd | QEMU VM mirror of krach |
| **krach-utm** | aarch64-linux | Niri + greetd | UTM VM mirror of krach (Apple Silicon) |
| **qemu** | x86_64-linux | Minimal | Bare QEMU/KVM test VM |
| **utm** | aarch64-linux | Minimal | Bare UTM test VM (Apple Silicon) |
| **demo-vm-01** | x86_64-linux | Batch | Demo fleet VM 1 |
| **demo-vm-02** | x86_64-linux | Batch | Demo fleet VM 2 |
| **edge-01** | x86_64-linux | Edge | Edge batch host 1 |
| **edge-02** | x86_64-linux | Edge | Edge batch host 2 |
| **edge-03** | x86_64-linux | Edge | Edge batch host 3 |
| **test-minimal** | x86_64-linux | Minimal | Test matrix: minimal role |
| **test-server** | x86_64-linux | Server | Test matrix: server role |
| **test-workstation-x86_64** | x86_64-linux | Workstation | Test matrix: workstation role |

## Scopes

Hosts declare flags in `hostSpecValues`. Scope modules auto-activate:

| Flag | Scope | What it enables |
|------|-------|-----------------|
| `!isMinimal` | `scopes/catppuccin.nix` | Catppuccin Macchiato theming across all apps |
| `!isMinimal` | `scopes/nix-index.nix` | command-not-found + comma (`, cowsay hello`) |
| `isGraphical` | `scopes/graphical/` | Pipewire, fonts, XDG portals, browsers, editors |
| `isDev` | `scopes/dev/` | Direnv, mise, dev packages (nodejs, python, docker, claude-code) |
| `useHyprland` | `scopes/desktop/hyprland.nix` | Hyprland WM + waybar, wofi, tofi, hyprlock |
| `useNiri` | `scopes/desktop/niri.nix` | Niri compositor + Noctalia Shell (NixOS-only) |
| `useGnome` | `scopes/desktop/gnome.nix` | GNOME desktop + GDM |
| `useGdm` | `scopes/display/gdm.nix` | Standalone GDM (without GNOME) |
| `useGreetd` | `scopes/display/greetd.nix` | Greetd display manager |
| `isImpermanent` | `scopes/impermanence.nix` | Ephemeral root, persist paths, btrfs wipe |
| `hasBluetooth` | `scopes/hardware/bluetooth.nix` | Bluetooth + Blueman |
| `useSecureBoot` | `scopes/hardware/secure-boot.nix` | Lanzaboote Secure Boot |
| `isDarwin` | `scopes/darwin/` | Homebrew, karabiner, aerospace |
| `useVpn` | `scopes/enterprise/vpn.nix` | Corporate VPN client (WireGuard/OpenVPN) |
| `useFilesharing` | `scopes/enterprise/filesharing.nix` | Samba/CIFS file sharing and network drives |
| `useLdap` | `scopes/enterprise/auth.nix` | LDAP/AD authentication (sssd/PAM) |
| `usePrinting` | `scopes/enterprise/printing.nix` | Network printing (CUPS + auto-discovery) |
| `useCorporateCerts` | `scopes/enterprise/certificates.nix` | Corporate CA trust and client certificates |
| `useProxy` | `scopes/enterprise/proxy.nix` | System-wide HTTP/HTTPS proxy |

## Portable Environments

Portable composites using [nix-wrapper-modules](https://github.com/BirdeeHub/nix-wrapper-modules). Work anywhere Nix is installed — configs bundled from `_config/` (same source as local HM).

```sh
# Full dev shell with all tools + configs (zsh, starship, git, neovim, etc.)
nix run github:abstracts33d/fleet#shell

# Configured kitty terminal launching the dev shell
nix run github:abstracts33d/fleet#terminal

# Test locally from within the flake
nix run .#shell              # enter the portable shell
nix run .#terminal           # launch kitty with the portable shell
nix build .#shell --dry-run  # verify it builds without running

# Verify bundled configs
nix run .#shell -- -c 'echo $STARSHIP_CONFIG && echo $GIT_CONFIG_GLOBAL'

# List all packages
nix flake show | grep packages
```

| Package | Description |
|---------|-------------|
| **`shell`** | Full zsh environment: starship, git, neovim, helix, btop, tmux, zellij, fzf + zsh plugins (fzf-tab, vi-mode, syntax-highlighting, autosuggestions) |
| **`terminal`** | Kitty terminal wrapping the shell environment |

## Prerequisites

- An SSH ed25519 key (`~/.ssh/id_ed25519`) with access to the [fleet-secrets](https://github.com/abstracts33d/fleet-secrets) repo
- [Nix](https://install.determinate.systems/nix) installed (macOS only — NixOS targets get it via nixos-anywhere)

## Installing

### macOS

```sh
# 1. Install Nix
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

# 2. Open a new terminal, then:
nix run github:abstracts33d/fleet#install -- -h <hostname> -u <username>
```

The script handles: SSH key verification, hostname setup, cloning the config, building, and activating.

### NixOS (from your workstation)

1. Boot the target machine — either:
   - NixOS minimal ISO (for bare metal/VM) — set root password with `passwd`
   - Any existing Linux with SSH access (nixos-anywhere will kexec into NixOS)

2. From your workstation (with Nix installed):
   ```sh
   nix run github:abstracts33d/fleet#install -- --target root@<ip> -h <hostname> -u <username>
   ```

3. After reboot, SSH in: `ssh <username>@<ip>` (password is managed by agenix)

> [!CAUTION]
> This will reformat the target's drive.

> [!NOTE]
> For Nvidia cards, boot the ISO with `nomodeset`.

## Day-to-day

```sh
# Rebuild and switch to new generation
nix run .#build-switch

# Rollback (macOS only)
nix run .#rollback

# Update secrets after changing fleet-secrets repo
nix flake update secrets

# Format all Nix files
nix fmt
```

```sh
# Validate everything (formatting + all builds)
nix run .#validate
```

> [!NOTE]
> Files must be tracked by git before builds: `git add .`
>
> Pre-commit hooks are auto-configured when entering `nix develop`.

## Adding a New Host

1. Add the host to `modules/fleet.nix` using `mkHost`:
   ```nix
   (mkHost {
     hostName = "<name>";
     org = abstracts33d;
     platform = "x86_64-linux"; # or aarch64-linux, aarch64-darwin
     role = builtinRoles.workstation; # or server, minimal, vm-test, edge, darwin-workstation
     hardwareModules = [ ./_hardware/<name>/disk-config.nix ];
     hostSpecValues = {
       # Override role defaults or add flags: useHyprland, hasBluetooth, wifiNetworks, etc.
     };
   })
   ```
2. Create `modules/_hardware/<name>/disk-config.nix` (use templates from `_shared/disk-templates/`)
3. `git add . && nix run .#install -- --target root@<ip> -h <name>`

Scopes auto-activate based on the flags set by the role and any per-host overrides — no feature lists needed.

## Adding a New Scope

1. Create `modules/scopes/<scope>/<nixos|home>.nix`
2. Define a deferred module gated by a hostSpec flag:
   ```nix
   {...}: {
     flake.modules.nixos.<name> = { config, lib, ... }: let
       hS = config.hostSpec;
     in {
       config = lib.mkIf hS.<flag> { ... };
     };
   }
   ```
3. Add the flag to `modules/_shared/host-spec-module.nix` if it's new
4. All hosts with that flag automatically get the scope — no wiring needed

## Virtual Machines

VM hosts use `mkVmHost` (virtio hardware, SPICE, software rendering, global DHCP):

| Host | Platform | Profile | Use |
|------|----------|---------|-----|
| `krach-qemu` | x86_64 | Niri + greetd | Test krach desktop in QEMU |
| `krach-utm` | aarch64 | Niri + greetd | Test krach desktop in UTM |
| `qemu` | x86_64 | Minimal | Bare test VM |
| `utm` | aarch64 | Minimal | Bare test VM |

### QEMU/KVM (Linux host)

```sh
# Download the ISO (first time only)
curl -L -o iso/nixos-x86_64.iso https://channels.nixos.org/nixos-unstable/latest-nixos-minimal-x86_64-linux.iso

# Terminal 1: Boot VM from ISO (graphical with virgl + SPICE)
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso

# Terminal 2: Install NixOS to the VM
nix run .#install -- --target root@localhost -p 2222 -h krach-qemu

# After install: close the QEMU/SPICE windows, then boot from disk
nix run .#spawn-qemu

# Headless mode (no GPU required)
nix run .#spawn-qemu -- --console

# Fully automated VM test (build ISO, install, verify)
nix run .#test-vm -- -h krach-qemu
```

Options: `--ram 8192`, `--cpus 4`, `--disk /path/to/disk.qcow2`, `--ssh-port 2223`

> [!NOTE]
> Graphical mode requires sudo on non-NixOS hosts (to set up OpenGL drivers). Use `--console` for headless.

### UTM (macOS host)

```sh
# Download the ISO (first time only)
curl -L -o iso/nixos-aarch64.iso https://channels.nixos.org/nixos-unstable/latest-nixos-minimal-aarch64-linux.iso

# Get UTM setup instructions
nix run .#spawn-utm -- --iso iso/nixos-aarch64.iso

# In UTM: create VM with displayed settings, boot it, then set root password:
#   passwd

# Find the VM IP (inside the VM):
#   ip addr show

# Install from your Mac:
nix run .#install -- --target root@<vm-ip> -h krach-utm
```

## Secrets Management

Secrets are managed with [agenix](https://github.com/ryantm/agenix). Encrypted secrets live in a private [fleet-secrets](https://github.com/abstracts33d/fleet-secrets) repo.

```sh
# Edit a secret
EDITOR="nvim" agenix -e output.age

# Update secrets input after changes
nix flake update secrets
```

## Architecture

See [TECHNICAL.md](TECHNICAL.md) for detailed architecture documentation.

NixFleet uses [flake-parts](https://flake.parts) + [import-tree](https://github.com/vic/import-tree) in a dendritic pattern:

- **`flake.nix`** is minimal — just inputs + `mkFlake` + `import-tree ./modules`
- **Every `.nix` file** under `modules/` is auto-imported as a flake-parts module
- **`_` prefixed** directories are excluded from auto-import
- **`fleet.nix`** defines all hosts centrally via `mkFleet` — no individual host files
- **Deferred modules** are auto-included by `mkNixosHost`/`mkDarwinHost` (called internally by `mkFleet`) via `builtins.attrValues`
- **Scope modules** self-activate with `lib.mkIf` based on `hostSpec` flags
- **HM** manages all local tool configs with catppuccin auto-theming
- **Wrappers** are for portable composites only (shell, terminal)
