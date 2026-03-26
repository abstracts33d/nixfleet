# lab

## Purpose

Headless NixOS server. No graphical environment, no dev tools. Runs core services only with impermanence.

## Location

- `modules/fleet.nix` (host entry via `mkHost`)
- `modules/_hardware/lab/disk-config.nix`

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Constructor | `mkFleet` -> `mkNixosHost` (internal) |
| User | <username> |
| Network interface | enp0s1 |
| Server mode | Yes |
| Impermanent | Yes |
| Dev tools | No |
| Graphical | No |

## Active Scopes

base (minimal subset), impermanence. No catppuccin/nix-index (server mode).

## Dependencies

- Hardware: `_hardware/lab/disk-config.nix` (no hardware-configuration -- headless)
- Secrets: `github-ssh-key`, `github-signing-key`, `user-password`, `root-password`

## Links

- [Host Overview](README.md)
- [Impermanence scope](../scopes/impermanence.md)
