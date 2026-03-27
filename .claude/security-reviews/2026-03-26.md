# Security Review — 2026-03-26

**Reviewer:** Claude Sonnet 4.6 (security-reviewer agent)
**Scope:** Full repository — focused on drift since 2026-03-25
**Previous review:** 2026-03-25

---

## Findings

| # | Severity | File | Finding | Status |
|---|----------|------|---------|--------|
| 1.1 | Medium | `modules/core/nixos.nix:72` | `TODO` in `fleet.nix` line 72: secrets path `shashed-password-file` still references wrong name in nix-secrets repo. No actual secret decryption regression, but the TODO comment in code is a sign it remains unresolved. | Unchanged (pending nix-secrets rename) |
| 2.1 | Medium | `modules/core/nixos.nix:252-276` | Org deny list pattern matching is glob-based — commands like `bash -c 'rm -rf /'` bypass patterns. Inherent limitation of Claude Code pattern matching. | Unchanged (acceptable limitation) |
| 3.1 | Medium | `modules/apps.nix:442` | SPICE on port 5900 with `disable-ticketing=on`. Implicitly localhost-bound by QEMU defaults but not explicit in args. | Unchanged (documented as acceptable for local dev) |
| 3.2 | Medium | `modules/apps.nix:234` | SSH `StrictHostKeyChecking=accept-new` in install script (TOFU). Inherent to provisioning. | Unchanged (documented as acceptable) |
| 4.1 | Low | `modules/core/darwin.nix:54-57` | Darwin `trusted-users` not gated by `!isServer` (NixOS was fixed, Darwin was not). Low risk since Darwin hosts are unlikely servers. | Unchanged (unfixed from 2026-03-25) |
| 4.2 | Low | `modules/apps.nix:525` | `test-vm` uses `StrictHostKeyChecking=no` — acceptable for ephemeral test VMs. | Unchanged (acceptable) |
| 5.1 | Low | `modules/scopes/dev/home.nix:26` | `defaultMode = "bypassPermissions"` + `skipDangerousModePermissionPrompt = true` in user HM config. Intentional for power user workflow, mitigated by org deny list. | Unchanged (intentional) |
| **6.1** | **High** | `modules/scopes/nixfleet/control-plane.nix` + `control-plane/src/routes.rs` | **Control plane API has no authentication.** All admin endpoints (`POST /set-generation`, `POST /register`, `PATCH /lifecycle`, `POST /report`) accept unauthenticated requests. Default listen is `0.0.0.0:8080`. Any host that can reach port 8080 can register machines, set desired generations, and manipulate fleet state. | **New** |
| **6.2** | **Medium** | `modules/scopes/nixfleet/control-plane.nix:16-18` | Default `listen = "0.0.0.0:8080"` binds to all interfaces. Should default to `127.0.0.1:8080` to prevent accidental exposure. The `openFirewall` option defaults to `false`, which mitigates exposure, but the bind itself is broader than needed. | **New** |
| **6.3** | **Medium** | `modules/scopes/nixfleet/agent.nix:95` | Agent systemd service has `ReadWritePaths = ["/var/lib/nixfleet" "/nix/var/nix"]`. The `/nix/var/nix` path grants write access to Nix daemon state directories (e.g. `/nix/var/nix/db`, `/nix/var/nix/daemon-socket`). A compromised agent could corrupt Nix store metadata. The agent only needs to invoke `nix copy` and `nixos-rebuild` — it should not need direct write to `/nix/var/nix`. | **New** |
| **6.4** | **Low** | `modules/fleet.nix:503-510`, `modules/fleet.nix:524-531` | `demo-vm-01` and `demo-vm-02` connect to control plane via plaintext `http://10.0.2.2:8080`. Agent-to-control-plane traffic (which includes machine IDs, generation hashes, and success/failure reports) is unencrypted. Acceptable for demo VMs on QEMU NAT, but should be documented. | **New** |

---

## Comparison with Previous Review (2026-03-25)

### New (4 findings)
| # | Severity | Finding |
|---|----------|---------|
| 6.1 | High | Control plane API: no authentication on any endpoint |
| 6.2 | Medium | Control plane: default bind `0.0.0.0:8080` (should be `127.0.0.1`) |
| 6.3 | Medium | Agent service: `ReadWritePaths` includes `/nix/var/nix` (overly broad) |
| 6.4 | Low | demo-vm-01/02: plaintext HTTP to control plane |

### Resolved (0 findings)
None resolved since 2026-03-25.

### Unchanged (7 findings)
- 1.1: `shashed-password-file` typo (pending nix-secrets rename)
- 2.1: Deny list pattern limitations (acceptable)
- 3.1: SPICE no-auth (documented)
- 3.2: Install SSH TOFU (documented)
- 4.1: Darwin trusted-users not gated (Low, unfixed)
- 4.2: test-vm StrictHostKeyChecking=no (acceptable)
- 5.1: bypassPermissions + skipDangerousModePermissionPrompt (intentional)

---

## Summary

| | Count |
|-|-------|
| New | 4 (1 High, 2 Medium, 1 Low) |
| Resolved | 0 |
| Unchanged | 7 |
| **Total open** | **11** (1 High, 4 Medium, 4 Low, 2 acceptable) |

---

## Action Items

### Priority 1 (High — block production deployment)
1. **Add authentication to the control plane API** (`modules/scopes/nixfleet/control-plane.nix`, `control-plane/src/`). Minimum viable: a shared secret token in an HTTP header, provisioned via agenix. Both the control plane and each agent need the same token. Without this, any machine on the same network can hijack fleet state.

### Priority 2 (Medium)
2. **Change default `listen` to `127.0.0.1:8080`** in `control-plane.nix`. Operators who need external access can override. Defense-in-depth even if auth is added.
3. **Narrow agent `ReadWritePaths`** — remove `/nix/var/nix`. The agent calls `nix copy` and `nixos-rebuild switch` via subprocess; those processes inherit their own permissions. The agent process itself does not need write access to Nix daemon state.
4. **Rename `shashed-password-file` in nix-secrets repo** (low-risk rename, has been pending since 2026-03-24).

### Priority 3 (Low)
5. **Gate Darwin `trusted-users` with `!isServer`** in `core/darwin.nix` for consistency with NixOS module.
6. **Document plaintext HTTP for demo VMs** in comments (or add a `warn` log in the agent when TLS is not in use).
