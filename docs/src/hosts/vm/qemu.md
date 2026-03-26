# qemu

## Purpose

Minimal QEMU/KVM test VM. No graphical environment, no dev tools. Used for quick validation of core NixOS config (SSH, networking, users).

## Location

- `modules/fleet.nix` (host entry via `mkHost` with `isVm = true`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Constructor | `mkFleet` -> `mkVmHost` (internal) |
| User | s33d |
| Minimal | Yes |
| Graphical | No (via isMinimal) |
| Dev tools | No (via isMinimal) |

## Usage

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso
nix run .#install -- --target root@localhost -p 2222 -h qemu
nix run .#test-vm                                       # default host
```

## Links

- [VM Overview](README.md)
- [spawn-qemu](../../apps/spawn-qemu.md)
