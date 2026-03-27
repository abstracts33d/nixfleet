# Multi-Repository Dependencies

NixFleet is the framework hub. Related repos are referenced via flake inputs or adjacent checkouts.

## fleet (reference implementation)
- **Location:** `../nixos-config` or `../fleet` (adjacent checkout)
- **Contains:** Org fleet config (fleet.nix, _hardware/, _config/, demo/)
- **Relationship:** Consumes `inputs.nixfleet.flakeModules.default`
- **Claude config:** Minimal overlay only — full .claude/ lives here in nixfleet

## fleet-secrets (private)
- **Input:** `inputs.secrets` (git+ssh://git@github.com/abstracts33d/fleet-secrets.git)
- **Contains:** Encrypted `.age` files (SSH keys, passwords, WiFi connections)
- **Workflow:** Edit in fleet-secrets → commit → push → `nix flake update secrets` in fleet repo

## When modifying secrets
1. Never output decrypted content
2. Always commit in fleet-secrets first, then update the flake input in fleet
3. Use `/secrets` skill for guided operations
4. After `nix flake update secrets`, verify build in fleet repo
