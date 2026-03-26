# krach-utm

## Purpose

UTM virtual machine mirroring the krach desktop on Apple Silicon. Uses aarch64-linux with UTM-specific hardware modules. Dev tools disabled.

## Location

- `modules/fleet.nix` (host entry via `mkHost` with `isVm = true`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | aarch64-linux |
| Constructor | `mkFleet` -> `mkVmHost` (internal) |
| User | <username> |
| Compositor | Niri |
| Display manager | greetd |
| Impermanent | Yes |
| Dev tools | No |

Overrides default VM hardware via `vmHardwareModules`:
- `platform = "aarch64-linux"`
- UTM-specific disk-config + hardware-configuration

## Usage

```sh
nix run .#spawn-utm -- --iso iso/nixos-aarch64.iso
nix run .#install -- --target root@<vm-ip> -h krach-utm
```

## Dependencies

- Hardware: `_hardware/utm/disk-config.nix`, `_hardware/utm/hardware-configuration.nix`

## Links

- [VM Overview](README.md)
- [spawn-utm](../../apps/spawn-utm.md)
