# NixFleet Roadmap

**Last updated:** 2026-03-31

Ordered execution plan across all design specs. Each phase has a clear "done when" and dependencies.

## Phase 0: Simplification

**Repo:** nixfleet + fleet
**Spec:** `superpowers/specs/2026-03-31-nixfleet-simplification-design.md`
**Decisions:** ADR-001, ADR-002, ADR-003, ADR-004, ADR-005
**Blocks:** Phase 2 (open-source)

Refactor the Nix layer from a 4-function DSL to a single `mkHost` API that produces standard `nixosConfigurations`.

### Deliverables

- [x] New `mkHost` in nixfleet — returns `nixosSystem`/`darwinSystem` directly
- [x] Remove mkFleet, mkOrg, mkRole, mkBatchHosts, mkTestMatrix
- [x] Remove deferred module registration — scopes become plain NixOS modules
- [x] Remove install, build-switch, docs apps
- [x] Agent/CP as `services.nixfleet-agent` / `services.nixfleet-control-plane` (standard NixOS service modules)
- [x] VM helpers exported as `nixfleet.lib.mkVmApps`
- [x] Update exports: `nixfleet.lib.mkHost`, `nixfleet.packages`, `nixfleet.nixosModules`, `nixfleet.diskoTemplates`
- [x] Update eval tests and VM tests
- [x] Update `examples/client-fleet/` + add standalone-host and batch-hosts examples
- [x] Migrate fleet repo: rewrite flake.nix, extract hosts, convert scopes to plain modules
- [x] Verify: nixos-anywhere, nixos-rebuild, darwin-rebuild, nix flake check, VM tests
- [x] Full documentation overhaul across both repos

### Done when

- `nixos-anywhere --flake .#web-01 root@<ip>` provisions a machine from scratch
- `sudo nixos-rebuild switch --flake .#web-01` rebuilds locally
- `darwin-rebuild switch --flake .#mac-01` works
- `nix flake check` passes in both repos
- No custom deployment scripts needed

---

## Phase 1: Rust Hardening

**Repo:** nixfleet
**Spec:** `superpowers/specs/2026-03-28-nixfleet-gtm-solo-design.md` — Phase 1
**Blocks:** Phase 2 (open-source)

Make the control plane deployable on a real network.

### Deliverables

- [x] mTLS agent-to-CP authentication (`tls.rs`, `--client-ca` / `--client-cert` / `--client-key`)
- [x] API key auth for operators (SHA-256 hashed, scoped: readonly/deploy/admin) (`auth.rs`, `V2__api_keys.sql`)
- [x] TLS-only control plane (HTTPS via axum-rustls, agent refuses HTTP unless `--allow-insecure`) (`main.rs`)
- [x] Audit log table + `GET /api/v1/audit` endpoint + CSV export (`audit.rs`, `V3__audit_events.sql`)
- [x] DB migrations via refinery (3 versioned SQL migrations, auto-apply on startup) (`db.rs`)
- [x] All mutations logged with actor identity (`routes.rs` logs Actor on every write)

### Done when

- ~~Agent authenticates to CP via client TLS certificate~~ Done
- ~~Operator authenticates via API key with correct permission scope~~ Done
- ~~HTTP connections refused in production mode~~ Done
- ~~Audit log records every write operation with identity~~ Done
- ~~DB schema managed by versioned migrations~~ Done

---

## Phase 2: Open Source

**Repo:** nixfleet
**Spec:** `superpowers/specs/2026-03-28-nixfleet-gtm-solo-design.md` — Phase 2
**Requires:** Phase 0 (clean API) + Phase 1 (hardened product) — both complete

Ship nixfleet as a credible open-source project.

### Deliverables

- [ ] Repository public on GitHub
- [ ] README: what, why, quickstart
- [ ] CONTRIBUTING.md
- [ ] License: AGPL-3.0 (control plane), MIT (framework/agent)
- [ ] Clean secrets/personal paths
- [ ] `docs/getting-started.md`: CP + 2 agents in 15 minutes
- [ ] `docs/architecture.md`: diagrams, state machine, protocol
- [ ] `docs/fleet-definition.md`: mkHost API + fleet repo structure + hostSpec reference
- [ ] Landing page (GitHub Pages)
- [ ] Community posts: NixOS Discourse, Hacker News
- [ ] NixCon 2026 talk proposal

### Done when

- A stranger can `nix flake init -t nixfleet`, define one host, and deploy it with `nixos-anywhere`
- Getting-started guide takes < 15 minutes to complete
- Repo has LICENSE, README, CONTRIBUTING, docs/

---

## Phase 3: Framework Infrastructure

**Repo:** nixfleet (optional modules) + fleet (enablement and host-specific config)
**Spec:** `superpowers/specs/2026-03-31-infrastructure-modules-design.md`
**Independent of:** Phase 2

Generic infrastructure modules that any fleet consumer can enable. Implemented as optional NixOS modules in nixfleet (`nixfleet.nixosModules.*`), configured per-host in the consuming fleet.

### Deliverables

#### nixfleet side (reusable modules)

- [ ] `nixfleet.nixosModules.attic-server` — thin wrapper around `services.atticd` with sane defaults (local filesystem backend, agenix secret paths, impermanence integration)
- [ ] `nixfleet.nixosModules.attic-client` — configures `nix.settings.substituters` + `trusted-public-keys` + `attic` CLI package
- [ ] `nixfleet.nixosModules.microvm-host` — wraps `microvm.host` with TAP networking defaults, virtiofs, and resource limit presets
- [ ] Flake inputs: `attic` (github:zhaofengli/attic), `microvm` (github:astro/microvm.nix)
- [ ] Eval tests for each module
- [ ] Documentation in `docs/src/scopes/`

#### fleet side (enablement)

- [ ] Enable `attic-server` on lab host with agenix-managed token and signing key
- [ ] Enable `attic-client` on all fleet hosts
- [ ] Post-build hook to push closures to Attic after successful rebuilds
- [ ] microvm VM definitions for isolated services / CI runners on a fleet host

### Done when

- `nix build` pulls from local Attic cache on all fleet hosts
- `attic push` works from any fleet host
- microvm VMs run on a fleet host with TAP networking
- A new nixfleet consumer can enable Attic or microvm with 3-5 lines of config

---

## Phase 4: Consulting + Enterprise

**Repo:** nixfleet + business
**Spec:** `superpowers/specs/2026-03-28-nixfleet-gtm-solo-design.md` — Phase 4
**Requires:** Phase 2 (open-source credibility)

NIS2 consulting pipeline feeding product development.

### Deliverables

- [ ] NIS2 consulting positioning and one-pager
- [ ] LinkedIn profile positioned for NIS2 + sovereign infrastructure
- [ ] Audit report template
- [ ] 3 pilot engagements (5-10 machines each)
- [ ] Pilot feedback → enterprise feature backlog
- [ ] Enterprise split: open-source vs licensed features (multi-tenant CP, RBAC, compliance reporting, PostgreSQL, dashboard)

### Done when

- 3 pilot case studies completed
- Clear list of enterprise features validated by 2+ clients
- Revenue sufficient to fund 2-3 months of dedicated enterprise dev

---

## Dependency Graph

```
Phase 0 (Simplification) ──┐
                            ├── Phase 2 (Open Source) ── Phase 4 (Consulting + Enterprise)
Phase 1 (Rust Hardening) ──┘
        ✓ DONE                      ← NEXT

Phase 3 (Framework Infrastructure) ── independent, nixfleet modules + fleet enablement
```

Phase 0 and 1 are both complete. Phase 2 (Open Source) is unblocked and the next priority. Phase 3 spans both repos: reusable modules in nixfleet, enablement in fleet. Phase 4 builds on open-source credibility for consulting and enterprise.

