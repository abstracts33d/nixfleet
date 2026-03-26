# utm

## Purpose

Minimal UTM test VM for Apple Silicon. No graphical environment, no dev tools. Used for quick aarch64-linux validation.

## Location

- `modules/fleet.nix` (host entry via `mkHost` with `isVm = true`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | aarch64-linux |
| Constructor | `mkFleet` -> `mkVmHost` (internal) |
| User | <username> |
| Minimal | Yes |
| Graphical | No (via isMinimal) |
| Dev tools | No (via isMinimal) |

Overrides default VM hardware via `vmHardwareModules`:
- `platform = "aarch64-linux"`
- UTM-specific disk-config + hardware-configuration

## Links

- [VM Overview](README.md)
- [spawn-utm](../../apps/spawn-utm.md)
