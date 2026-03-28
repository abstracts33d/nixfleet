# nixfleet-agent scope

## Purpose

NixOS module that runs the NixFleet fleet management agent as a systemd service. The agent polls the control plane, fetches new generations, and applies them via `nixos-rebuild`.

## Location

- `modules/scopes/nixfleet/agent.nix`

## Activation

This scope is **not flag-activated**. It is a deferred NixOS module registered as `flake.modules.nixos.nixfleet-agent`. Enable it explicitly per host:

```nix
services.nixfleet-agent.enable = true;
```

## Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | false | Enable the agent service |
| `controlPlaneUrl` | str | — | URL of the NixFleet control plane (required) |
| `machineId` | str | `hostname` | Machine identifier sent to control plane |
| `pollInterval` | int | 300 | Poll interval in seconds |
| `cacheUrl` | str or null | null | Binary cache URL for pre-fetching closures |
| `dbPath` | str | `/var/lib/nixfleet/state.db` | SQLite state database path |
| `dryRun` | bool | false | Check and fetch but do not apply generations |

## Systemd Hardening

The service runs with NoNewPrivileges, PrivateTmp, PrivateDevices, and restricted read-write paths (`/var/lib/nixfleet`, `/nix/var/nix`). This is a security-sensitive service — hardening is intentional.

## Impermanence

When `hostSpec.isImpermanent` is true, `/var/lib/nixfleet` is automatically added to `environment.persistence."/persist".directories` so agent state survives reboots.

## Links

- [Scopes Overview](README.md)
- [Control Plane](nixfleet-control-plane.md)
