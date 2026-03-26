# krach

## Purpose

Main workstation running NixOS with the Niri scrollable-tiling Wayland compositor, greetd display manager, and full dev tools. Uses impermanence with btrfs root wipe.

## Location

- `modules/fleet.nix` (host entry via `mkHost`)
- `modules/_hardware/krach/disk-config.nix`
- `modules/_hardware/krach/hardware-configuration.nix`

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Constructor | `mkFleet` -> `mkNixosHost` (internal) |
| User | <username> |
| Network interface | enp6s0 |
| Compositor | Niri + Noctalia Shell |
| Display manager | greetd (tuigreet) |
| Impermanent | Yes (btrfs wipe on boot) |
| Dev tools | Yes (Docker, direnv, mise, Claude Code) |
| WiFi networks | home |

## Extra Packages

krach adds host-specific packages via `extraHmModules`:
- `jetbrains.ruby-mine`
- `slack`

With impermanence persist paths for JetBrains config/data/cache.

## Active Scopes

Based on flags: catppuccin, nix-index, base, graphical, dev, niri, greetd, impermanence.

## Dependencies

- Hardware: `_hardware/krach/` (disk-config + hardware-configuration)
- Secrets: `github-ssh-key`, `github-signing-key`, `user-password`, `root-password`, `wifi-home`
- VM mirror: [krach-qemu](vm/krach-qemu.md) for testing

## Links

- [Host Overview](README.md)
- [Niri scope](../scopes/desktop/niri.md)
- [Impermanence scope](../scopes/impermanence.md)
- [Dev scope](../scopes/dev.md)
