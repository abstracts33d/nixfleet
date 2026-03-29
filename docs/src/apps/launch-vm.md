# launch-vm

## Purpose

Build, install, and launch a graphical QEMU VM with SPICE display for visual verification. Uses a persistent disk that survives reboots.

## Usage

```sh
nix run .#launch-vm                          # launch krach-qemu (default)
nix run .#launch-vm -- -h ohm                # launch a different host config
nix run .#launch-vm -- --rebuild             # wipe disk and reinstall from scratch
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `-h HOST` | `krach-qemu` | Host configuration to build |
| `--rebuild` | off | Wipe disk and reinstall from scratch |
| `--ram MB` | `4096` | RAM in MB |
| `--cpus N` | `2` | CPU count |
| `--ssh-port N` | `2222` | SSH forward port |

## Platform

Linux only (requires KVM).

## How it works

1. If no disk exists (or `--rebuild`): builds the host, creates a qcow2 disk, installs via nixos-anywhere
2. Boots the VM with SPICE display (opens `remote-viewer` automatically)
3. Disk is persisted at `~/.local/share/nixfleet/vms/<host>.qcow2`

## Differences from spawn-qemu

| | `spawn-qemu` | `launch-vm` |
|---|---|---|
| Display | Headless (SSH only) | Graphical (SPICE) |
| Install | Requires pre-built ISO | Builds and installs automatically |
| Use case | CI / automated testing | Visual verification / dev |

## Links

- [Apps Overview](README.md)
- [spawn-qemu](spawn-qemu.md)
- [test-vm](test-vm.md)
