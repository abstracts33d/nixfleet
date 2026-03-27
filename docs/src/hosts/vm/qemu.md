# qemu (framework test VM)

## Purpose

Minimal framework test VM. Tests the `isMinimal` flag — no base packages, no dev tools, no graphical. Used for quick validation of core NixOS config (SSH, networking, users) and the VM test suite.

## Location

- `modules/fleet.nix` (host entry via `mkHost` with `isVm = true`)
- `modules/_hardware/qemu/` (QEMU hardware config)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Organization | test-org |
| Constructor | `mkFleet` → `mkVmHost` (internal) |
| User | testuser (from org defaults) |
| isMinimal | true |

## What it tests

- `isMinimal = true` suppresses base packages
- VM tests (`vm-core`, `vm-minimal`) use this host's configuration

## Usage

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso   # first boot
nix run .#install -- --target root@localhost -p 2222 -h qemu
nix run .#test-vm                                      # default host
```

## Links

- [VM Overview](README.md)
- [spawn-qemu](../../apps/spawn-qemu.md)
- [test-vm](../../apps/test-vm.md)
