# Secrets Bootstrap

## Purpose

During NixOS installation, the agenix decryption key must be provisioned to the target machine before secrets can be decrypted. The install script handles this automatically via nixos-anywhere's `--extra-files`.

## Location

- `modules/apps.nix` (install script, extra-files section)

## Bootstrap Flow

1. Install script looks for decryption key at `~/.keys/id_ed25519` (preferred) or `~/.ssh/id_ed25519` (fallback)
2. Creates temp directory structure: `<tmp>/persist/home/<user>/.keys/id_ed25519`
3. Passes to nixos-anywhere via `--extra-files <tmp>`
4. nixos-anywhere copies the key to the target's `/persist/home/<user>/.keys/` during installation
5. On first boot, agenix finds the key and decrypts all secrets

## Key Locations

| Path | Purpose | Persisted |
|------|---------|-----------|
| `~/.keys/id_ed25519` | Agenix decryption key | Yes (impermanence) |
| `~/.ssh/id_ed25519` | Runtime SSH key (agenix-managed) | No (ephemeral) |
| `/persist/home/<user>/.keys/` | Persist bind mount source | Yes |

## Security Notes

- The decryption key is copied from the installer's machine to the target
- It's the same ed25519 key used for SSH and GitHub access
- On impermanent hosts, an activation script ensures correct ownership of `.keys/`

## Links

- [Secrets Overview](README.md)
- [Install app](../apps/install.md)
- [Impermanence](../scopes/impermanence.md)
