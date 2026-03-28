# krach (framework test host)

## Purpose

Framework test host for org defaults, SSH hardening, and impermanence tests. Declared in `modules/fleet.nix` as a VM-mode host for CI eval purposes.

> **Note:** This is the framework's *test* host. The physical `krach` workstation (with Niri, dev tools, etc.) is defined in the [fleet overlay](https://github.com/abstracts33d/fleet).

## Location

- `modules/fleet.nix` (host entry via `mkHost`)
- `modules/_hardware/qemu/` (shared QEMU hardware config)

## Configuration

| Property | Value |
|----------|-------|
| Platform | x86_64-linux |
| Organization | test-org |
| Constructor | `mkFleet` → `mkVmHost` (internal) |
| User | testuser (from org defaults) |
| isImpermanent | true |

## What it tests

- Org defaults are inherited (timezone, locale, SSH keys)
- `hostSpec.userName` is set from org
- Impermanence scope activates (btrfs wipe, persist paths)
- SSH hardening is applied (PermitRootLogin=prohibit-password)

## Links

- [Host Overview](README.md)
- [Impermanence scope](../scopes/impermanence.md)
