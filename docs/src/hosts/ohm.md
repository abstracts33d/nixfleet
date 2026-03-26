# ohm

## Purpose

Secondary laptop running NixOS with GNOME desktop and GDM. French keyboard layout. No dev tools (used for daily non-development tasks).

## Location

- `modules/fleet.nix` (host entry via `mkHost`)
- `modules/_hardware/ohm/disk-config.nix`
- `modules/_hardware/ohm/hardware-configuration.nix`

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Constructor | `mkFleet` -> `mkNixosHost` (internal) |
| User | sabrina |
| Network interface | enp2s0 |
| Desktop | GNOME |
| Display manager | GDM (auto via useGnome) |
| Impermanent | Yes |
| Dev tools | No |

## Extra Config

Uses `extraModules` to override keyboard layout:
- `xserver.xkb.layout = "fr,us"` (French primary, US secondary)
- `console.keyMap = "fr"`

## Active Scopes

catppuccin, nix-index, base, graphical, gnome, gdm, impermanence.

## Dependencies

- Hardware: `_hardware/ohm/` (disk-config + hardware-configuration)
- Secrets: `github-ssh-key`, `github-signing-key`, `user-password`, `root-password`

## Links

- [Host Overview](README.md)
- [GNOME scope](../scopes/desktop/gnome.md)
- [GDM scope](../scopes/display/gdm.md)
