# WiFi Bootstrap

## Purpose

Provision WiFi connections from agenix secrets on first boot. Each network maps to a `wifi-<name>.age` file in nix-secrets containing a NetworkManager `.nmconnection` file.

## Location

- `modules/core/nixos.nix` (bootstrap-wifi systemd service)
- `modules/_shared/host-spec-module.nix` (`wifiNetworks` option)

## How It Works

1. Host declares `wifiNetworks = ["home"];` in hostSpecValues
2. Agenix decrypts `wifi-home.age` to `/run/agenix/wifi-home`
3. `bootstrap-wifi` systemd service runs:
   - After `agenix.service`, before `NetworkManager.service`
   - Checks if `.nmconnection` file already exists in NM directory
   - If absent, copies from agenix secret and sets mode 600
4. NetworkManager picks up the connection on start

## Target Directories

| Host type | NM connections path |
|-----------|-------------------|
| Impermanent | `/persist/system/etc/NetworkManager/system-connections` |
| Standard | `/etc/NetworkManager/system-connections` |

## Creating WiFi Secrets

```sh
# Export existing connection
sudo cat /etc/NetworkManager/system-connections/<name>.nmconnection > wifi-home.nmconnection

# Encrypt with age
age -R ~/.ssh/id_ed25519.pub -o wifi-home.age wifi-home.nmconnection

# Add to nix-secrets repo, commit, update
nix flake update secrets
```

## Current State

The `wifiNetworks` option is implemented but secrets have not yet been created in nix-secrets (see TODO.md).

## Dependencies

- Depends on: agenix, NetworkManager
- Host flag: `wifiNetworks` list in hostSpec
- Used by: [krach](../hosts/krach.md) (`wifiNetworks = ["home"]`)

## Links

- [Secrets Overview](README.md)
- [NixOS core](../core/nixos.md)
