# Security Model

How this config handles security across multiple layers.

## Defense in Depth

Security is layered, not centralized:

1. **Encrypted secrets** — agenix encrypts everything sensitive
2. **Ephemeral root** — impermanent hosts wipe on boot, reducing attack surface
3. **SSH hardening** — restricted authentication methods, no root login
4. **Firewall** — default deny inbound
5. **Claude Code permissions** — 3-level model prevents dangerous operations

## The 3-Level Permissions Model

Claude Code permissions are layered. Higher levels cannot override lower levels:

| Level | What | Who controls it |
|-------|------|----------------|
| Organization | Non-overridable deny list (destructive ops, privilege escalation) | NixOS managed policy |
| Project | Repo-specific tool allowlist | Git-tracked settings |
| User | Personal preferences and default mode | Home Manager |

The org deny list blocks operations like `rm -rf /`, `sudo`, force push to main, and nix store manipulation. Even with `bypassPermissions` enabled, these are blocked.

## Secrets Security

- Secrets are never in the Nix store (world-readable)
- Encrypted at rest in a private repository
- Decrypted to ephemeral paths at boot
- Decryption key lives in the persist partition only

## Network Security

- Firewall enabled with default deny
- SSH uses key-only authentication
- No unnecessary ports exposed
- SPICE (for VMs) is localhost-only

## Security Reviews

Regular security reviews are documented in `.claude/security-reviews/`. The review process checks secrets, permissions, network, VMs, supply chain, and impermanence.

## Further Reading

- [Secrets Management](../concepts/secrets.md) — how secrets work
- [Technical Security Details](../../claude/permissions.md) — Claude Code permissions
