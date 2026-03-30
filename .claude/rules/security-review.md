# Security Review

## Schedule
Run a comprehensive security review:
- After every major feature merge (new scopes, new integrations, new hosts)
- Before any deployment to production hardware
- At minimum once per month

## 3-Level Permissions Model
Claude Code permissions are layered. Higher levels cannot override lower levels.

| Level | Location | Managed by | Purpose |
|-------|----------|-----------|---------|
| **1. Org (managed policy)** | `/etc/claude-code/settings.json` | NixOS `environment.etc` in `core/nixos.nix` | Non-overridable deny list (security floor) |
| **2. Project** | `.claude/settings.json` | Git-tracked | Repo-specific allow list (nix, git, alejandra, ssh) + hooks |
| **3. User** | `~/.claude/settings.json` | HM `programs.claude-code` in `scopes/dev/home.nix` | Personal allow list + defaultMode preference |

The org deny list blocks destructive ops (rm -rf, dd, mkfs, shred), privilege escalation (sudo, pkexec, doas, su), dangerous git (force push, hard reset, clean -fd), and nix store manipulation. This cannot be bypassed even with `bypassPermissions`.

## Secrets Management

The framework is **secrets-agnostic** -- `hostSpec.secretsPath` is an optional hint. The reference fleet uses **agenix**:
- Encrypted `.age` files in private repo (`nix-secrets`), referenced as `inputs.secrets`
- Decrypted at activation time to `/run/agenix/`, symlinked to target paths
- Decryption key at `~/.keys/id_ed25519` (persisted via impermanence)
- On Darwin, identity path must point to `~/.keys/` not `~/.ssh/` (avoids circular dependency)

Key rules:
- `.ssh` and `.gnupg` are **ephemeral** -- never persisted. Only `known_hosts` persisted as a file.
- Agenix re-decrypts on every boot -- no stale secrets
- `hashedPasswordFile` uses agenix paths under `/run/agenix/` (not nix store)
- Never output decrypted content

## String Interpolation Safety

Never interpolate NixOS option values directly into shell strings without escaping.

```nix
# GOOD
ExecStart = lib.concatStringsSep " " [
  "${pkg}/bin/cmd"
  "--url" (lib.escapeShellArg cfg.controlPlaneUrl)
  "--count" (toString cfg.count)
];

# BAD -- user-supplied values can break commands
ExecStart = "... --flag ${cfg.userInput}";
```

Safe: `lib.escapeShellArg`, `toString intValue`, `${pkg}/bin/name` (store paths).

## SSH Hardening

Applied in `core/nixos.nix`: `PermitRootLogin = "prohibit-password"`, `PasswordAuthentication = false`, `KbdInteractiveAuthentication = false`. Firewall enabled on all hosts, no ports opened by default.

## NIS2 Compliance (S7 -- planned)

EU Directive 2022/2555. NixOS satisfies most obligations by construction:
- **Traceability**: every config change is a git commit
- **Incident recovery**: generations are immutable; rollback is atomic (<90s)
- **Supply chain**: `flake.lock` pins by content hash (SHA-256)
- **Asset inventory**: `nixosConfigurations` IS the inventory (no CMDB drift)

NixFleet value-add: SBOM generation, vulnerability scanning, incident timeline, compliance score, exportable reports.

## Process
1. Review secrets management (agenix paths, permissions, nix store exposure)
2. Review 3-level permission model (org deny list, project allows, user preferences, hooks)
3. Review network security (firewall, SSH hardening, exposed ports)
4. Review VM security (SPICE auth, SSH TOFU)
5. Review supply chain (inputs, follows, lock file freshness)
6. Review impermanence (persist paths, activation scripts, boot scripts)
7. Produce a timestamped report in `.claude/security-reviews/`

## Report Format
Save as `.claude/security-reviews/YYYY-MM-DD.md` with:
- Date, reviewer, scope
- Findings table (severity, file, description, status)
- Comparison with previous review (new/resolved/unchanged)
- Action items with priority

---

## Historical Findings

Org-specific findings are stored in `.claude/security-reviews/` as timestamped records.
See `findings-2026-03-24.md` for the abstracts33d reference fleet review.
