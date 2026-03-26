# Scope System

## Purpose

Scopes are feature modules that self-activate based on `hostSpec` flags. Each scope registers deferred modules via `config.flake.modules.{nixos,darwin,homeManager}.<name>` and gates its config with `lib.mkIf hS.<flag>`. Adding a new scope file automatically applies to all hosts with the matching flag.

## Location

- `modules/scopes/` -- all scope modules
- `modules/_shared/host-spec-module.nix` -- flag definitions and smart defaults

## Scope Activation Table

| Flag | Scope | Description |
|------|-------|-------------|
| `!isMinimal` | [catppuccin](catppuccin.md) | Macchiato + lavender theming |
| `!isMinimal` | [nix-index](nix-index.md) | command-not-found + comma |
| `!isMinimal` | [base](base.md) | Universal CLI packages |
| `isGraphical` | [graphical](graphical.md) | Pipewire, fonts, browsers, editors |
| `isDev` | [dev](dev.md) | Docker, direnv, mise, Claude Code |
| `useNiri` | [niri](desktop/niri.md) | Niri compositor + Noctalia Shell |
| `useHyprland` | [hyprland](desktop/hyprland.md) | Hyprland WM + waybar, wofi |
| `useGnome` | [gnome](desktop/gnome.md) | GNOME desktop (trimmed) |
| `useGreetd` | [greetd](display/greetd.md) | TUI greeter |
| `useGdm` | [gdm](display/gdm.md) | GNOME Display Manager |
| `isImpermanent` | [impermanence](impermanence.md) | Ephemeral root + btrfs wipe |
| `hasBluetooth` | [bluetooth](hardware/bluetooth.md) | Bluetooth + Blueman |
| `useSecureBoot` | [secure-boot](hardware/secure-boot.md) | Lanzaboote Secure Boot |
| `isDarwin` | [homebrew](darwin/homebrew.md) | Homebrew casks and brews |
| `isDarwin` | [karabiner](darwin/karabiner.md) | Key remapping |
| `useAerospace` | [aerospace](darwin/aerospace.md) | AeroSpace WM |
| `useVpn` | [vpn](enterprise/vpn.md) | WireGuard/OpenVPN client |
| `useFilesharing` | [filesharing](enterprise/filesharing.md) | Samba/CIFS network drives |
| `useLdap` | [auth](enterprise/auth.md) | LDAP/AD authentication |
| `usePrinting` | [printing](enterprise/printing.md) | CUPS + auto-discovery |
| `useCorporateCerts` | [certificates](enterprise/certificates.md) | Corporate CA trust |
| `useProxy` | [proxy](enterprise/proxy.md) | System-wide HTTP/HTTPS proxy |

## Roles: Pre-Composed Flag Bundles

Roles (`modules/_shared/lib/roles.nix`) bundle hostSpec flags into named presets. When a host or batch template uses a role, the role's flags determine which scopes activate. Six built-in roles:

| Role | Flags | Primary scopes activated |
|------|-------|--------------------------|
| `workstation` | isDev, isGraphical, isImpermanent, useNiri | dev, graphical, niri, greetd, impermanence |
| `server` | isServer, !isDev, !isGraphical | base (minimal subset) |
| `minimal` | isMinimal | (none beyond core) |
| `vm-test` | !isDev, isGraphical, isImpermanent, useNiri | graphical, niri, greetd, impermanence |
| `edge` | isServer, isMinimal | (none beyond core) |
| `darwin-workstation` | isDarwin, isDev, isGraphical | dev, graphical (HM only), homebrew, karabiner |

Roles are assigned via `mkHost` or `mkBatchHosts` in `fleet.nix`. Individual `hostSpecValues` can override any role default.

## Adding a New Scope

1. Create `modules/scopes/<scope>.nix`
2. Register deferred modules gated by a hostSpec flag
3. Add the flag to `host-spec-module.nix` if new
4. All matching hosts automatically get the scope

## Persist Paths Pattern

Impermanence persist paths live alongside their program definitions, not in a central file. Each scope adds its own `home.persistence."/persist".directories` when `isImpermanent` is true.

## Links

- [Architecture](../architecture.md)
- [Host System](../hosts/README.md)
