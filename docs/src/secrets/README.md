# Secrets Management

## Purpose

Age-encrypted secrets managed via agenix. Encrypted secrets live in a private repo (`nix-secrets`), referenced as a non-flake input. Secrets are decrypted at boot and symlinked to their target paths.

> **Framework note:** NixFleet is secrets-agnostic. Agenix is the `abstracts33d` org's implementation choice, wired in via org modules (`fleet.nix` nixosModules and darwinModules). A different org could use sops-nix or Vault instead.

## Location

- `modules/fleet.nix` -- org nixosModules/darwinModules inject agenix config
- `modules/_shared/keys.nix` -- SSH public keys (org-specific)

## Architecture

```
nix-secrets repo (private, git+ssh)
  |-- github-ssh-key.age
  |-- github-signing-key.age
  |-- <user>-hashed-password-file
  |-- shashed-password-file (root)
  |-- wifi-<name>.age
```

### Decryption key
- Located at `~/.keys/id_ed25519`
- Persisted via impermanence (bind mount from `/persist`)
- On impermanent hosts, agenix checks both `~/.keys/id_ed25519` and `/persist/home/<user>/.keys/id_ed25519`

### Secret targets
- `github-ssh-key` -> `~/.ssh/id_ed25519` (symlink, mode 600)
- `github-signing-key` -> `~/.ssh/pgp_github.key` (symlink on NixOS, copy on Darwin)
- `user-password` -> `/run/agenix/user-password` (root-owned, for `hashedPasswordFile`)
- `root-password` -> `/run/agenix/root-password`
- `wifi-<name>` -> `/run/agenix/wifi-<name>` (consumed by bootstrap service)

### Ephemeral design
`.ssh` and `.gnupg` are **not persisted** -- agenix re-creates them each boot. Only `.ssh/known_hosts` is persisted as a file.

## Managing secrets

```sh
# Edit a secret
EDITOR="nvim" agenix -e output.age

# Update after changes
nix flake update secrets
```

## Links

- [Bootstrap](bootstrap.md)
- [WiFi](wifi.md)
- [NixOS core](../core/nixos.md)
- [Impermanence](../scopes/impermanence.md)
