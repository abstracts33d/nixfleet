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
