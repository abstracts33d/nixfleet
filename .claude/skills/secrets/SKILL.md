---
name: secrets
description: Manage agenix secrets across the fleet repo and secrets repo. Use for adding, rekeying, listing, or syncing encrypted secrets.
user-invocable: true
---

# Secrets Management

Orchestrates operations across two repos:
- **Fleet repo** (this repo) — declares secrets in `modules/core/nixos.nix`
- **Secrets repo** — stores encrypted `.age` files (private repo, detected from flake input `secrets`)

The fleet-secrets repo location is detected from the flake input:
```bash
nix flake metadata --json | jq -r '.locks.nodes.secrets.locked.url'
```
Or found via `nix eval .#inputs.secrets.outPath`.

## Commands

### /secrets add <name>
Add a new secret:
1. Ask for the secret content or file path
2. Encrypt with `age -R` using the repo's public keys
3. Save to `fleet-secrets/<name>.age`
4. Commit in fleet-secrets
5. Add `age.secrets."<name>"` declaration in `modules/core/nixos.nix`
6. Run `nix flake update secrets` in this repo
7. Commit

### /secrets rekey
Re-encrypt all secrets with current keys:
1. Find fleet-secrets repo
2. Run `cd <fleet-secrets> && agenix --rekey`
3. Commit and push fleet-secrets
4. `nix flake update secrets` in this repo
5. Commit

### /secrets list
Show declared vs available secrets:
1. Parse `age.secrets` from `modules/core/nixos.nix`
2. List `.age` files in fleet-secrets
3. Show: declared + available, declared but missing, available but undeclared

### /secrets sync
Verify consistency between repos:
1. Check all declared secrets have corresponding `.age` files
2. Check flake.lock secrets revision matches fleet-secrets HEAD
3. Report any drift

## Rules
- Never output decrypted secret content
- Always commit in fleet-secrets BEFORE updating the flake input
- Ask confirmation before any destructive operation (rekey, delete)
