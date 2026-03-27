# Secrets Management

Knowledge about secrets, encryption, and the agenix workflow.

## Framework vs Org Secrets

The framework is **secrets-agnostic** -- `hostSpec.secretsPath` is an optional hint; no secrets manager is imposed. The reference fleet uses **agenix** as its secrets implementation, configured in org modules (`fleet.nix` nixosModules/darwinModules). Other orgs may use sops-nix, vault, or any other secrets manager.

## Agenix Workflow

1. Encrypted secrets (`.age` files) live in a private repo (`nix-secrets`)
2. Referenced as flake input: `inputs.secrets`
3. Decrypted at activation time to `/run/agenix/`
4. Symlinked to target paths (e.g., `~/.ssh/`)

### Decryption Key

- Lives at `~/.keys/id_ed25519` (persisted via impermanence)
- Provisioned during install via `nixos-anywhere --extra-files`
- On Darwin, identity path must point to `~/.keys/` not `~/.ssh/` (avoids circular dependency)

### Secret Types

| Secret | Path | Notes |
|--------|------|-------|
| SSH private key | `/run/agenix/github-ssh-key` -> `~/.ssh/id_ed25519` | Ephemeral, re-decrypted each boot |
| SSH signing key | `/run/agenix/github-signing-key` -> `~/.ssh/signing_key` | Ephemeral |
| User password | `/run/agenix/user-password` | Referenced by `hashedPasswordFile` |
| Root password | `/run/agenix/root-password` | Referenced by `rootHashedPasswordFile` |
| WiFi secrets | `/run/agenix/wifi-<name>` | Copied to NetworkManager on first boot |

### Key Points

- `.ssh` and `.gnupg` directories are **ephemeral** -- never persisted
- Only `known_hosts` is persisted (as a file, not directory)
- Agenix re-decrypts on every boot -- no stale secrets
- WiFi bootstrap: per-host `wifiNetworks` list maps to `wifi-<name>.age` files

## Nix Store Exposure

Hashed passwords were previously in the nix store (world-readable). Now resolved: `hashedPasswordFile` uses agenix paths under `/run/agenix/`, which are permission-controlled.

## Modifying Secrets

1. Never output decrypted content
2. Always commit in nix-secrets first, then update the flake input
3. After `nix flake update secrets`, verify build
4. Rekeying: `agenix --rekey` when SSH keys change
