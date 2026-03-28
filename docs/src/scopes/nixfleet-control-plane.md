# nixfleet-control-plane scope

## Purpose

NixOS module that runs the NixFleet control plane as a systemd service. The control plane is an Axum HTTP server that maintains a machine registry and serves generation manifests to agents.

## Location

- `modules/scopes/nixfleet/control-plane.nix`

## Activation

This scope is **not flag-activated**. It is a deferred NixOS module registered as `flake.modules.nixos.nixfleet-control-plane`. Enable it explicitly per host:

```nix
services.nixfleet-control-plane.enable = true;
```

## Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | false | Enable the control plane service |
| `listen` | str | `0.0.0.0:8080` | Listen address and port |
| `dbPath` | str | `/var/lib/nixfleet-cp/state.db` | SQLite state database path |
| `openFirewall` | bool | false | Open the listen port in the firewall |

## Systemd Hardening

The service runs with NoNewPrivileges, PrivateTmp, PrivateDevices, and restricted read-write paths (`/var/lib/nixfleet-cp`).

## Impermanence

When `hostSpec.isImpermanent` is true, `/var/lib/nixfleet-cp` is automatically added to `environment.persistence."/persist".directories` so control plane state survives reboots.

## Links

- [Scopes Overview](README.md)
- [Fleet Agent](nixfleet-agent.md)
