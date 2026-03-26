# Secrets Management

How sensitive data is handled without storing it in plain text.

## The Problem

System configs need secrets: SSH keys, passwords, WiFi credentials. But Nix store paths are world-readable — you cannot put secrets there.

## The Solution: agenix (abstracts33d's choice)

> **Framework note:** NixFleet itself is secrets-agnostic. The `abstracts33d` organization chose [agenix](https://github.com/ryantm/agenix), wired in through org modules. Your organization could use sops-nix, Vault, or any other tool.

This config uses [agenix](https://github.com/ryantm/agenix) for secrets:

1. Secrets are encrypted with age using SSH public keys
2. Encrypted files live in a private `secrets repo` repository
3. At boot, agenix decrypts secrets using `~/.keys/id_ed25519`
4. Decrypted secrets are placed in ephemeral locations (`~/.ssh/`, etc.)

## Bootstrap Flow

During installation:
1. Your SSH key is copied to `~/.keys/id_ed25519` on the target
2. This key can decrypt all secrets for that host
3. On first boot, agenix decrypts everything automatically

## What Gets Encrypted

- SSH private keys
- Hashed user passwords
- WiFi credentials (per-network, per-host)
- GitHub tokens and other API keys

## Integration with Impermanence

Secrets work naturally with ephemeral roots:
- The decryption key (`~/.keys/`) is in the persist partition
- Decrypted secrets (`~/.ssh/`) are ephemeral — recreated each boot
- No risk of stale secrets accumulating

## Updating Secrets

When the secrets repo repo changes:

```sh
nix flake update secrets
nix run .#build-switch
```

## Further Reading

- [Technical Secrets Details](../../secrets/README.md) — paths, keys, bootstrap
- [Installation](../getting-started/installation.md) — how keys are provisioned
