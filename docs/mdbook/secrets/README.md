# Secrets Management

## Framework Approach

NixFleet is **secrets-agnostic**. The framework does not bundle any secrets tool — it provides clean extension points via `hostSpec` options for fleet repos to plug in their preferred secrets management.

The framework test fleet has no secrets at all (`hashedPasswordFile = null`).

## Extension Points

| Mechanism | Location | Purpose |
|-----------|----------|---------|
| `hostSpec.secretsPath` | `host-spec-module.nix` | Pass secrets repo path to modules without hardcoding |
| `hostSpec.hashedPasswordFile` | `host-spec-module.nix` | Wired to `users.users.<name>.hashedPasswordFile` |
| `hostSpec.rootHashedPasswordFile` | `host-spec-module.nix` | Wired to `users.users.root.hashedPasswordFile` |

## Wiring Secrets in a Fleet

Fleet repos import their chosen secrets tool and wire it via `mkHost` modules. For example:

```nix
# agenix
modules = [ inputs.agenix.nixosModules.default ./modules/secrets.nix ];

# sops-nix
modules = [ inputs.sops-nix.nixosModules.sops ./modules/secrets.nix ];
```

The `secrets.nix` module defines encrypted file paths, decryption identity paths, and output locations. The framework's `hostSpec` options provide the connection points.

## Links

- [Bootstrap](bootstrap.md) — provisioning keys during installation
- [WiFi](wifi.md) — WiFi credential provisioning patterns
- [Impermanence scope](../scopes/impermanence.md) — how secrets interact with ephemeral roots
