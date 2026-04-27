# `modules/scopes/nixfleet/_cache-server.nix`

NixOS service module for the NixFleet binary cache server (harmonia).
Thin wrapper around the upstream services.harmonia NixOS module.
Serves paths directly from the local Nix store over HTTP.
Auto-included by mkHost (disabled by default).

## Bindings

### `services.harmonia.cache`

Delegate to upstream harmonia NixOS module

