---
name: deploy
description: Safe build-switch with validation and rollback. Only user can trigger this.
user-invocable: true
disable-model-invocation: true
---

# Deploy

Safe deployment with validation gates and automatic rollback.

## Process

1. **Pre-flight**: Dispatch `test-runner` → `nix run .#validate`
   - If ANY test fails → STOP. Show errors. Do not proceed.
2. **Build + Switch**: `nix run .#build-switch`
3. **Smoke check**: Verify critical services are running
   - `systemctl is-active multi-user.target`
   - `systemctl is-active sshd`
   - `systemctl is-active NetworkManager`
4. **If smoke fails**: `nix run .#rollback` automatically, report what failed
5. **If success**: Report generation info and active services

## Safety
- This skill has `disable-model-invocation: true` — only the user can trigger it
- Always validate before switching
- Always smoke-check after switching
- Always rollback on failure
