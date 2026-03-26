# krach-qemu

## Purpose

QEMU/KVM virtual machine mirroring the krach workstation desktop (Niri + greetd). Used for testing the graphical desktop environment without touching real hardware. Dev tools disabled to reduce build time.

## Location

- `modules/fleet.nix` (host entry via `mkHost` with `isVm = true`)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Constructor | `mkFleet` -> `mkVmHost` (internal) |
| User | <username> |
| Compositor | Niri |
| Display manager | greetd |
| Impermanent | Yes |
| Dev tools | No |

## Usage

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso   # first boot
nix run .#install -- --target root@localhost -p 2222 -h krach-qemu
nix run .#spawn-qemu                                    # after install
nix run .#test-vm -- -h krach-qemu                      # automated test
```

## Dependencies

- Hardware: default QEMU hardware (provided by `mkVmHost` internally)
- Mirrors: [krach](../krach.md) (same compositor, different hardware)

## Links

- [VM Overview](README.md)
- [spawn-qemu](../../apps/spawn-qemu.md)
- [test-vm](../../apps/test-vm.md)
