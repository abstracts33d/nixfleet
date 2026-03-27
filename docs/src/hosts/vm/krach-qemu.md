# krach-qemu (framework test VM)

## Purpose

Framework test VM for scope activation tests and SSH hardening checks. Uses impermanence. Declared in `modules/fleet.nix` with `isVm = true`.

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
| isImpermanent | true |

## What it tests

- SSH hardening (PermitRootLogin, PasswordAuthentication, firewall)
- `hostSpec.organization` and `hostSpec.role` options exist
- `nixfleet.extensions` is empty by default
- Scope activation (impermanence scope activates via `isImpermanent`)

## Usage

```sh
nix run .#spawn-qemu -- --iso iso/nixos-x86_64.iso   # first boot
nix run .#install -- --target root@localhost -p 2222 -h krach-qemu
nix run .#spawn-qemu                                    # after install
nix run .#test-vm -- -h krach-qemu                      # automated test
```

## Links

- [VM Overview](README.md)
- [spawn-qemu](../../apps/spawn-qemu.md)
- [test-vm](../../apps/test-vm.md)
