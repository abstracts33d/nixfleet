# Virtual Machine Hosts

## Purpose

VM hosts are declared in `modules/fleet.nix` with `isVm = true`. The `mkFleet` API internally calls `mkVmHost`, which wraps `mkNixosHost` with VM-specific defaults: virtio hardware, SPICE guest agent, software rendering (`LIBGL_ALWAYS_SOFTWARE=1`), and global DHCP.

## Location

- `modules/fleet.nix` -- all VM host entries (via `mkHost` with `isVm = true`)
- `modules/_shared/mk-host.nix` -- `mkVmHost` internal constructor

## Framework VM Hosts

| Host | Platform | Flags | Purpose |
|------|----------|-------|---------|
| [krach-qemu](krach-qemu.md) | x86_64-linux | `isImpermanent` | Scope activation + SSH hardening tests |
| [qemu](qemu.md) | x86_64-linux | `isMinimal` | Minimal / VM test suite default host |

> Fleet overlay VM hosts (`krach-utm`, `utm`) for aarch64/UTM are defined in the [fleet repo](https://github.com/abstracts33d/fleet).

## mkVmHost Defaults (internal)

- Hardware: `_hardware/qemu/disk-config.nix` + `_hardware/qemu/hardware-configuration.nix`
- Platform: `x86_64-linux` (overridable for aarch64)
- Extra NixOS modules: SPICE agent, force global DHCP, software rendering, mesa

## Links

- [Host Overview](../README.md)
- [spawn-qemu app](../../apps/spawn-qemu.md)
- [spawn-utm app](../../apps/spawn-utm.md)
- [test-vm app](../../apps/test-vm.md)
