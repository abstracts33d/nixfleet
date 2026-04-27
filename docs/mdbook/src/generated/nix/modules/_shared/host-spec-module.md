# `modules/_shared/host-spec-module.nix`

hostSpec - identity carrier for every host.

Framework-level options only - scope/role/profile/hardware concerns
live elsewhere:
- `nixfleet.<scope>.*` options come from `arcanesys/nixfleet-scopes`
- `fleet.*` options come from the consuming fleet

Posture flags (`isImpermanent`, `isServer`, `isMinimal`) that were
here in earlier revisions of NixFleet have been removed - their roles
are now played by scope `enable` options (set by roles) in
nixfleet-scopes.

## Bindings

### `hostName`

--- Identity ---

### `timeZone`

--- Locale / keyboard ---

### `rootHashedPasswordFile`

--- Access ---

### `networking`

--- Networking ---

### `secretsPath`

--- Secrets backend hint (backend-agnostic) ---

### `isDarwin`

--- Platform ---

