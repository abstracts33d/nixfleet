# Security Hardening

Knowledge about system hardening, firewall, SSH, and the permissions model.

## 3-Level Claude Code Permissions

| Level | Location | Managed by | Purpose |
|-------|----------|-----------|---------|
| **1. Org** | `/etc/claude-code/settings.json` | NixOS `environment.etc` in `core/nixos.nix` | Non-overridable deny list (security floor) |
| **2. Project** | `.claude/settings.json` | Git-tracked | Repo-specific allow list (nix, git, alejandra, ssh) |
| **3. User** | `~/.claude/settings.json` | HM in `scopes/dev/home.nix` | Personal allow list + defaultMode |

The org deny list blocks:
- Destructive ops: `rm -rf`, `dd`, `mkfs`, `shred`
- Privilege escalation: `sudo`, `pkexec`, `doas`, `su`
- Dangerous git: force push, hard reset, clean -fd
- Nix store manipulation

This cannot be bypassed even with `bypassPermissions`. The org deny list applies on NixOS only (not Darwin).

## SSH Hardening

Applied in `core/nixos.nix`:
- `PermitRootLogin = "prohibit-password"` (key-only for root)
- `PasswordAuthentication = false`
- `KbdInteractiveAuthentication = false`
- ISO uses `PermitRootLogin = "prohibit-password"` (hardened from `"yes"`)

## Firewall

- `networking.firewall.enable = true` on all NixOS hosts
- No ports opened by default
- Individual services open their own ports via NixOS firewall options

## Trusted Users

- `trusted-users` includes regular user, gated with `!isServer` on NixOS
- Darwin `trusted-users` not gated by `!isServer` (low risk -- Darwin hosts unlikely servers)

## Current Security Findings

From latest review (2026-03-25):

### Open (all Low/Medium, acceptable)
- Typo `shashed-password-file` in nix-secrets (pending rename)
- Org deny list pattern matching is glob-based (inherent limitation)
- SPICE without auth on port 5900 (documented, local dev only)
- SSH TOFU on install (inherent to provisioning)
- Darwin trusted-users not gated
- test-vm StrictHostKeyChecking=no (acceptable for ephemeral VMs)

### Resolved
- bypassPermissions deny list -> moved to org-level managed policy
- Firewall + SSH hardening added
- Hashed passwords encrypted with agenix
- `allowBroken = true` set to `false`
