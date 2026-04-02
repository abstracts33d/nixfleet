# Secrets Management

How sensitive data is handled without storing it in plain text.

## The Problem

System configs need secrets: SSH keys, passwords, WiFi credentials. But Nix store paths are world-readable — you cannot put secrets there.

## Framework Approach

NixFleet is secrets-agnostic. The framework provides extension points (`hostSpec.secretsPath`, `hostSpec.hashedPasswordFile`, etc.) but does not mandate a specific tool. Common choices include:

- [agenix](https://github.com/ryantm/agenix) — age-encrypted secrets, SSH key decryption
- [sops-nix](https://github.com/Mic92/sops-nix) — multi-format encrypted secrets (YAML, JSON, dotenv)
- [Vault](https://www.vaultproject.io/) — centralized secret management
- Plain NixOS `hashedPasswordFile` — for simple setups without a secrets tool

## Integration with Impermanence

Secrets work naturally with ephemeral roots:
- Decryption keys are placed in the persist partition
- Decrypted secrets are ephemeral — recreated each boot
- No risk of stale secrets accumulating

## Wiring Secrets in Your Fleet

Fleet repos import their chosen secrets tool and wire it via `mkHost` modules:

```nix
# Example: agenix
modules = [
  inputs.agenix.nixosModules.default
  ./modules/secrets.nix
];

# Example: sops-nix
modules = [
  inputs.sops-nix.nixosModules.sops
  ./modules/secrets.nix
];
```

The framework's `hostSpec.secretsPath` option provides a hint for where secrets live, but the actual wiring is entirely fleet-specific.

## Further Reading

- [Technical Secrets Details](../../secrets/README.md) — extension points and patterns
- [Secrets Bootstrap](../../secrets/bootstrap.md) — provisioning keys during installation
