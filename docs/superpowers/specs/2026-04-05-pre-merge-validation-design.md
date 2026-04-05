# Pre-Merge Validation: Agent Rebuild Guard + Integration Tests + Demo

**Date:** 2026-04-05
**Status:** Draft
**Scope:** nixfleet (agent guard + VM tests) + nixfleet-demo (validation branch)

## Context

Three PRs are open on nixfleet:

| PR | Branch | What it adds |
|----|--------|-------------|
| #19 | `feat/cli-mtls-bootstrap` | CLI mTLS, bootstrap, machines register, auto-registration + tag sync |
| #20 | `feat/phase3-framework-infra` | Attic server/client modules, MicroVM host, backup backends (restic/borg), firewall bridge |
| #21 | `feat/rollout-policies-history-schedule` | Named rollout policies, event history, scheduled rollouts |

Together these PRs close the "agent real rebuild" gap. The agent already implements the full state machine (fetch → apply → verify → rollback) with `nix copy --from` for fetching and `switch-to-configuration switch` for applying. PR #20 adds the Attic binary cache infrastructure that serves closures to agents.

Two paths exist for delivering closures to agents:

- **With Attic:** CLI pushes to Attic, CP sets `desired_generation` with `cache_url`, agent fetches via `nix copy --from`
- **Without Attic:** CLI pre-pushes via `nix copy --to ssh://agent`, CP sets `desired_generation` with no `cache_url`, agent finds closure already in store

This spec covers pre-merge validation of all 3 PRs plus a safety improvement to the agent.

## 1. Agent `nix path-info` Guard

### Problem

When no `cache_url` is provided, `fetch_closure()` in `agent/src/nix.rs` logs "No cache URL — assuming closure is available locally" and returns `Ok(())`. If the closure is not actually in the store, the agent proceeds to `Applying` and `switch-to-configuration` fails with a confusing error.

### Solution

Add a `nix path-info <store_path>` check in the no-cache branch of `fetch_closure()`. If the path does not exist, return an error immediately. The agent transitions to `Idle` and reports the failure to CP.

### Behavior

```
fetch_closure(store_path, cache_url):
  if cache_url is Some:
    nix copy --from <cache_url> <store_path>     # existing, unchanged
  else:
    nix path-info <store_path>                    # NEW
    if exit != 0:
      bail!("store path {store_path} not found locally and no cache URL configured")
    else:
      Ok(())                                      # path exists, proceed to apply
```

### File Changes

- `agent/src/nix.rs` — modify `fetch_closure()` no-cache branch
- `agent/src/nix.rs` — add unit test for path construction (integration tested in VM tests)

### State Machine Impact

None. The `Fetching` state already handles `Err` from `fetch_closure()` by transitioning to `Idle`. The guard just makes failures explicit and early instead of deferring to `switch-to-configuration`.

## 2. VM Integration Tests

New file: `modules/tests/vm-agent-rebuild.nix`

### Test A: Attic Pipeline (with cache)

**Nodes:** 3 (cp, attic, agent)

**Setup:**
- CP with TLS, API key auth
- Attic server with signing key, local storage
- Agent with Attic client configured (substituter + trusted key), mTLS client cert

**Steps:**
1. CP starts, Attic server starts
2. Agent registers via health report, reports healthy
3. A second NixOS system closure for the agent is pre-built (differs from the running system by adding `environment.etc."nixfleet-test-marker".text = "v2"`). Test driver pushes this closure to Attic.
4. Test driver pushes the closure to Attic (`attic push`)
5. Test driver sets `desired_generation` on CP via API (`POST /api/v1/machines/agent/set-generation` with `cache_url` pointing to Attic)
6. Agent polls, fetches from Attic via `nix copy --from`, applies via `switch-to-configuration switch`, health checks pass, reports success
7. Assert: `/etc/nixfleet-test-marker` contains "v2" on agent node
8. Assert: CP shows agent at new generation via `GET /api/v1/machines`

### Test B: No-Cache Path (pre-seeded)

**Nodes:** 2 (cp, agent)

**Setup:**
- CP with TLS, API key auth
- Agent with mTLS client cert, no Attic client

**Steps:**
1. CP starts, agent registers
2. Test driver copies a closure directly to agent's store (`nix copy --to ssh://agent`)
3. Test driver sets `desired_generation` on CP with no `cache_url`
4. Agent polls, `nix path-info` succeeds, applies, health checks pass, reports success
5. Assert: agent at new generation

### Test C: Missing Path Guard (negative)

**Nodes:** 2 (cp, agent) — same setup as Test B

**Steps:**
1. CP starts, agent registers
2. Test driver sets `desired_generation` to a fabricated store path (e.g. `/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixos-system-fake`) with no `cache_url`
3. Agent polls, `nix path-info` fails, agent transitions to Idle, reports error to CP
4. Assert: agent stays at original generation
5. Assert: CP received failure report

### Test Organization

All three tests in a single NixOS test file (`vm-agent-rebuild.nix`) as separate subtests sharing common node definitions where possible. Tests A and B prove the two delivery modes work. Test C proves the guard catches the failure case.

### Integration with Existing Tests

- `vm-fleet.nix` — existing 4-node fleet test (CP + 3 agents, rollout, pause/resume). Unchanged.
- `vm-agent-rebuild.nix` — new test focused on the agent rebuild pipeline. Complements vm-fleet by testing the actual system switch.
- Both registered as flake checks: `checks.x86_64-linux.vm-agent-rebuild`

## 3. nixfleet-demo Validation

### Flake Override

```nix
# nixfleet-demo/flake.nix — on validate/pre-merge branch
nixfleet.url = "path:/home/s33d/dev/nix-org/nixfleet-validate";
```

Points to the integration worktree that merges all 3 PRs + guard + tests.

### New Host: `cache-01`

| Property | Value |
|----------|-------|
| Role | Attic binary cache server |
| Platform | x86_64-linux |
| Impermanent | No (cache data must persist) |
| VLAN IP | 10.0.100.6 |
| SSH port | 2206 (deterministic from sorted hostname) |

```nix
cache-01 = mkHost {
  hostName = "cache-01";
  platform = "x86_64-linux";
  hostSpec = orgDefaults;
  modules = hostModules "cache-01" ++ [{
    services.nixfleet-attic-server = {
      enable = true;
      openFirewall = true;
      signingKeyFile = config.age.secrets.attic-signing-key.path;
    };
  }];
};
```

### Changes to Existing Hosts

**All agent hosts** (`web-01`, `web-02`, `db-01`, `mon-01`):
```nix
services.nixfleet-attic-client = {
  enable = true;
  cacheUrl = "http://cache-01:8081";
  publicKey = "<attic-public-key>";
};
```

**`db-01`** — add backup backend:
```nix
nixfleet.backup = {
  enable = true;  # already set
  backend = "restic";
  schedule = "*-*-* 03:00:00";  # already set
  restic = {
    repository = "/var/lib/backup/restic-repo";
    passwordFile = config.age.secrets.restic-password.path;
  };
};
```

### New Secrets (agenix)

| Secret | Used by |
|--------|---------|
| `attic-signing-key.age` | `cache-01` Attic server |
| `restic-password.age` | `db-01` backup |

### New/Updated Modules

| File | Change |
|------|--------|
| `modules/attic.nix` | New — wires Attic server secret on `cache-01`, Attic client on agent hosts |
| `modules/secrets.nix` | Add `attic-signing-key.age`, `restic-password.age` |
| `modules/vm-network.nix` | Add `cache-01` at `10.0.100.6`, update `/etc/hosts` |
| `fleet.nix` | Add `cache-01` definition, update `db-01` backup config, add `attic.nix` to `fleetModules` |
| `hosts/cache-01/` | New — `hardware-configuration.nix` + `disk-config.nix` (same pattern as other hosts) |

### Demo Walkthrough Additions

After existing bootstrap + auto-registration steps:

1. **Build and push to cache:** `nix build .#nixosConfigurations.web-01.config.system.build.toplevel` then `attic push demo <store-path>` on the host
2. **Create rollout policy:** `nixfleet policy create --name canary-web --strategy canary --batch-size 1,100 --failure-threshold 1 --on-failure pause --health-timeout 60`
3. **Deploy with policy:** `nixfleet deploy --generation <hash> --tag web --policy canary-web --cache-url http://cache-01:8081`
4. **Observe:** agent fetches from Attic, applies, health checks, CP advances batches
5. **Scheduled rollout:** `nixfleet deploy --generation <hash> --tag db --schedule-at "2026-04-06T03:00:00Z" --policy safe-db`
6. **View events:** `nixfleet rollout status <id>` shows event history timeline

## 4. Integration Worktree Strategy

### Setup (nixfleet)

1. Create worktree: `git worktree add ../nixfleet-validate validate/pre-merge`
2. Merge PR branches:
   ```
   git merge feat/cli-mtls-bootstrap
   git merge feat/phase3-framework-infra
   git merge feat/rollout-policies-history-schedule
   ```
3. Add commits on top: path-info guard, VM tests
4. Verify: `nix flake check --no-build` + `cargo test --workspace`

### Setup (nixfleet-demo)

1. Create branch: `validate/pre-merge`
2. Override flake input to `path:../nixfleet-validate`
3. Add `cache-01`, Attic config, backup backend, new secrets
4. Verify: `nix flake check --no-build`

### After Validation

| Commit | Cherry-pick to |
|--------|---------------|
| `nix path-info` guard | New PR #22 on main |
| VM test A (Attic pipeline) | PR #20 (depends on Attic modules) |
| VM test B + C (no-cache + negative) | PR #22 (depends on guard) |

nixfleet-demo stays on `validate/pre-merge` until PRs merge to main, then rebases and switches input back to `github:abstracts33d/nixfleet`.

## Success Criteria

1. `nix flake check --no-build` passes in both the nixfleet worktree and nixfleet-demo
2. `cargo test --workspace` passes in the nixfleet worktree (agent guard test)
3. `nix build .#checks.x86_64-linux.vm-agent-rebuild --no-link` passes all 3 subtests
4. nixfleet-demo `nix flake check --no-build` evaluates all 6 hosts cleanly
5. Manual VM walkthrough of the demo (build → push to Attic → deploy with policy → agent applies → health check → success) works end-to-end
