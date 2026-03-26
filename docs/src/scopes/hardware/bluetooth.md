# bluetooth

## Purpose

Bluetooth hardware support with Blueman GUI manager. Powers on Bluetooth at boot.

## Location

- `modules/scopes/hardware/bluetooth.nix`

## Configuration

**Gate:** `hasBluetooth`

### NixOS module
- `hardware.bluetooth.enable = true`
- `hardware.bluetooth.powerOnBoot = true`
- `services.blueman.enable = true`

## Dependencies

- Depends on: hostSpec `hasBluetooth` flag

## Links

- [Scope Overview](../README.md)
