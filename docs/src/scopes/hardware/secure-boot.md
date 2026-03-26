# secure-boot

## Purpose

Lanzaboote Secure Boot for NixOS. Replaces systemd-boot with a signed bootloader. Requires initial key enrollment (see lanzaboote quick start guide).

## Location

- `modules/scopes/hardware/secure-boot.nix`

## Configuration

**Gate:** `useSecureBoot`

### NixOS module
- `boot.lanzaboote.enable = true`
- `boot.lanzaboote.pkiBundle = "/etc/secureboot"`
- `boot.loader.systemd-boot.enable = lib.mkForce false` (lanzaboote replaces it)
- System packages: `sbctl` (Secure Boot key management)

### Impermanence
Persists `/etc/secureboot` (PKI bundle with signing keys).

## Setup

Follow the [Lanzaboote Quick Start](https://github.com/nix-community/lanzaboote/blob/main/docs/QUICK_START.md) for initial key enrollment.

## Dependencies

- Input: `lanzaboote` (github:nix-community/lanzaboote)
- Depends on: hostSpec `useSecureBoot` flag

## Links

- [Scope Overview](../README.md)
