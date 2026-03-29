# Apps

## Purpose

Flake apps defined in `modules/apps.nix` as `perSystem` shell scripts. Provide the primary CLI interface for installing, building, testing, and managing VMs.

## Location

- `modules/apps.nix`

## App Table

| App | Command | Platform | Description |
|-----|---------|----------|-------------|
| [install](install.md) | `nix run .#install` | All | macOS local + NixOS remote install via nixos-anywhere |
| [build-switch](build-switch.md) | `nix run .#build-switch` | All | Day-to-day rebuild and switch |
| [validate](validate.md) | `nix run .#validate` | All | Full validation suite |
| [docs](docs.md) | `nix run .#docs` | All | Serve documentation locally |
| [spawn-qemu](spawn-qemu.md) | `nix run .#spawn-qemu` | Linux | QEMU VM launcher (headless) |
| [launch-vm](launch-vm.md) | `nix run .#launch-vm` | Linux | Graphical VM with SPICE display |
| [test-vm](test-vm.md) | `nix run .#test-vm` | Linux | Automated ISO-to-verify cycle |
| [spawn-utm](spawn-utm.md) | `nix run .#spawn-utm` | Darwin | UTM VM setup guide |
| [rollback](rollback.md) | `nix run .#rollback` | Darwin | macOS generation rollback |

## DevShell

`apps.nix` also defines the default devShell (`nix develop`) with:
- `bashInteractive`, `git`, `age`
- shellHook: sets `EDITOR=vim` and activates git hooks (`.githooks/`)

## Links

- [Architecture](../architecture.md)
- [Testing](../testing/README.md)
