# Secrets Management

## Framework Approach

NixFleet is **secrets-agnostic**. The framework does not bundle agenix configuration or secrets — it provides clean extension points via `mkOrg nixosModules` and `hostSpec.secretsPath` for consuming fleet repos to plug in their secrets management of choice.

The framework test fleet has no secrets at all (`hashedPasswordFile = null`).

## Extension Points

| Mechanism | Location | Purpose |
|-----------|----------|---------|
| `mkOrg nixosModules` | `fleet.nix` | Inject org-level NixOS modules (agenix config, etc.) |
| `hostSpec.secretsPath` | `host-spec-module.nix` | Pass secrets repo path to modules without hardcoding |
| `hostSpec.hashedPasswordFile` | `host-spec-module.nix` | Wired to `users.users.<name>.hashedPasswordFile` |
| `hostSpec.rootHashedPasswordFile` | `host-spec-module.nix` | Wired to `users.users.root.hashedPasswordFile` |

## Reference Implementation

The [fleet overlay](https://github.com/abstracts33d/fleet) shows how to integrate agenix:

- Encrypted `.age` files in the private [fleet-secrets repo](https://github.com/abstracts33d/fleet-secrets)
- `mkOrg nixosModules` injects agenix module + secret path definitions
- Decryption key at `~/.keys/id_ed25519` (persisted via impermanence)
- Secrets: github SSH/GPG keys, hashed passwords, WiFi credentials

## `nix flake update secrets`

When using a secrets repo as a flake input (e.g. `inputs.secrets`), update it with:

```sh
nix flake update secrets
```

## Links

- [Bootstrap](bootstrap.md)
- [WiFi](wifi.md)
- [Impermanence scope](../scopes/impermanence.md)
