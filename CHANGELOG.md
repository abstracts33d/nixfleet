# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/). Versioning: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### v0.2 completeness cycle (2026-04-28)

Closes the framework-scoped gaps required for ARCHITECTURE.md §8 done-criterion #1 — *"destroying the CP's database and rebuilding from empty state results in full fleet visibility within one reconcile cycle"* — to hold against strict reading. Six commits between `fe3baec` and `ac5a66f`; tests 127 → 165.

#### Added

- **Wave soak timer (RFC-0002 §3.2 Healthy → Soaked).**
  - `Action::SoakHost { rollout, host }` variant on the reconciler's action stream.
  - Reconciler `Healthy` arm consults `rollout.last_healthy_since[host]` against `wave.soak_minutes`; emits `SoakHost` when `now - last_healthy_since >= soak_window`.
  - CP-side `host_rollout_state` table (V003 migration) keyed on `(rollout_id, hostname)` with `host_state` + `last_healthy_since` columns.
  - DB methods: `record_host_healthy`, `clear_host_healthy`, `host_soak_state_for_rollout`, `healthy_rollouts_for_host`, `mark_host_soaked`, `host_rollout_state_exists`.
  - CP-side action processor in `server::reconcile::apply_actions` runs each tick to fold `SoakHost` actions into the DB.
  - `Rollout` widened with `last_healthy_since: HashMap<String, DateTime<Utc>>` (additive, `#[serde(default)]` keeps file-backed `observed.json` fixtures parseable).
  - `db::active_rollouts_snapshot` joins `pending_confirms` (latest per host, state ∈ `{pending, confirmed}`) with `host_rollout_state` so `observed_projection::project` populates `active_rollouts` (was hardcoded `Vec::new()` pre-cycle).

- **Confirm-handler idempotency (gap A, #46).** `/v1/agent/confirm` with no matching pending row now cross-checks the agent's `closure_hash` against the verified target; match → synthetic `confirmed` row + `record_host_healthy` + 204. Mismatch → 410 (existing semantics). Closes the unnecessary-rollback regression on CP rebuild.

- **Signed `revocations.json` sidecar (gap C, #48).** New CONTRACTS.md §I artifact alongside `fleet.resolved.json`, signed by the same `ciReleaseKey`. CP fetches + verifies + replays into `cert_revocations` on every reconcile tick. Operator UX shifts revocations from CLI-on-CP to git commit + CI sign + push. Closes the only security-material rebuild gap.
  - New types: `nixfleet_proto::Revocations` + `RevocationEntry`.
  - New verify path: `nixfleet_reconciler::verify_revocations`.
  - New CP poll: `revocations_poll` module + `--revocations-artifact-url` / `--revocations-signature-url` / `--revocations-token-file` CLI flags.
  - Release-tool integration: optional `--revocations-attr <attr>` flag signs the operator-declared list alongside `fleet.resolved.json`.
  - Nix-side: `mkFleet` gains a `revocations` option; surfaced as `<flake>.fleet.revocations`.

- **Agent-attested `last_confirmed_at` (gap B-cp, #47 — CP-side half).** New optional field on `CheckinRequest` (wire-additive, no protocol bump). CP repopulates `host_rollout_state.last_healthy_since` from the attestation when the host is converged on its target with no existing `host_rollout_state` row, clamped to `min(now, attested)` against clock skew. Agent-side population (B-agent) folds into #2 when the agent activation loop lands.

- **`signed_fetch` module.** Shared `build_client` / `read_token` / `fetch_signed_pair` helpers extracted from `channel_refs_poll` + `revocations_poll` so the two parallel modules stay byte-stable on the HTTP fetch path.

- **End-to-end soak-loop integration test (`tests/soak_loop.rs`).** Single test exercises the full chain: `confirm` → `record_healthy` → projection → reconciler → `SoakHost` → `mark_soaked` → projection → `ConvergeRollout`.

#### Documentation

- **`docs/commercial-extensions.md`** (new). Catalogues capabilities the open kernel intentionally does not ship — HA replication, real-time signed-state snapshots, SLA observability, audit packages, hosted CP, multi-tenant federation, fine-grained RBAC, long-running metrics warehousing — with stranger-fleet-test rationale and integration paths.
- **ARCHITECTURE.md §6 Phase 10 — "CP-resident state by recovery profile"** subsection enumerating every SQLite table with its recovery class (soft from agent inputs / hard from signed artifacts in git).
- **ARCHITECTURE.md §7 Non-goals** points at `docs/commercial-extensions.md` for capabilities deliberately out of scope.
- **ARCHITECTURE.md §8 done-criterion #1** expanded with the per-table guarantee.
- **v0.2 completeness cycle landed** — gap #2 closed (steps 1+2+3); gaps A/B/C/D enumerated with their closing commits. Tracking moved to GitHub issues (#46/#47/#48/#14, plus open #68/#69/#67 for the remaining items).

#### Issues

- Closed: #46 (gap A), #48 (gap C).
- Updated: #47 (gap B — CP-side complete, agent-side defers to #2), #14 (Phase 10 teardown — acceptance criterion refreshed; microvm.nix scenario deferred to next cycle pending #5's harness work), #10 (v0.2 tracking — cycle summary), #12 (signed artifacts — cross-link to gap C), #2 (Magic rollback — naming the slot for B-agent).

### Architecture refactor — kernel/opinion split (2026-04-27 → 2026-04-28)

Two-repo architecture: framework + consumer fleet. `nixfleet-scopes` archived; its
contents folded into `nixfleet` (contract impls) and the consuming fleet
(service wraps, role bundles, hardware modules, platform shims).

#### Added

- **`contracts/`** (top-level) — schemas: `host-spec.nix`, `trust.nix`, `persistence.nix`. Moved out of `modules/` because import-tree treats `modules/` as flake-parts modules and the schemas' `assertions` declarations leak into flake-parts level if put inside.
- **`impls/`** (top-level) — pluggable contract impls absorbed from former `nixfleet-scopes`:
  - `impls/persistence/impermanence.nix` — btrfs root-wipe + impermanence module wiring. New options: `nixfleet.persistence.impermanence.{rootDevice, oldRootsRetentionDays}`.
  - `impls/keyslots/tpm/` — TPM-backed signing keyslot.
  - `impls/gitops/forgejo.nix` — channel-refs URL builder for Forgejo / Gitea.
  - `impls/secrets/default.nix` — backend-agnostic identity-path resolution.
- **`flake.scopes.<family>.<impl>`** — new public output exposing contract impls. Example: `inputs.nixfleet.scopes.persistence.impermanence`.
- **`impermanence`** flake input (required by `impls/persistence/impermanence.nix`; inert when that impl is not imported).

#### Changed

- **`lib/` consolidation.** `modules/_shared/lib/` collapsed into top-level `lib/`. Single entry: `lib/default.nix` is the wired entry (`{inputs, lib}`). `lib/mk-fleet.nix` is the pure entry (`{lib}`-only) for the canonicalize binary and eval-only tests.
- **File naming standardised** to kebab-case across the framework:
  - `lib/mkFleet.nix` → `lib/mk-fleet.nix` (function `mkFleet` unchanged).
  - `tests/lib/mkFleet/` → `tests/lib/mk-fleet/`.
  - `modules/scopes/nixfleet/_agent_darwin.nix` → `_agent-darwin.nix`.
- **Schemas relocated** to `contracts/` and renamed to drop the redundant `-module` suffix:
  - `modules/_trust.nix` → `contracts/trust.nix`.
  - `modules/_shared/host-spec-module.nix` → `contracts/host-spec.nix`.
  - `modules/scopes/nixfleet/_persistence.nix` → `contracts/persistence.nix`.
- **Framework `core/_*.nix` trimmed to true prerequisites only.** `_nixos.nix` keeps trust import + flake-mode `nix` settings + `hostSpec` → standard NixOS option pass-through + root SSH from `hostSpec`. `_darwin.nix` keeps `system.stateVersion`, `system.checks.verifyNixPath`, `system.primaryUser`, `hostSpec.isDarwin`. The opinions that used to ship from these (substituter lists, GC policy, openssh hardening, nixpkgs.config defaults, network baselines, Dock management, Determinate-Nix wiring, TouchID + pam-reattach) are now consumer-fleet responsibility.
- **Opinion-leak audit on docstrings, comments, and option examples.** `lab.internal` / `abstracts33d` / `krach` / `s33d` replaced with neutral examples (`example.com` / `myorg` / `test-host`); `/run/agenix/*` examples replaced with `/run/secrets/*` so the framework reads file paths backend-agnostically; `attic push fleet ...` typical-example expanded to list cache-server alternatives.
- **`secrets.identityPaths.userKey` default** changed from `${hS.home}/.keys/id_ed25519` to `${hS.home}/.ssh/id_ed25519` (universal NixOS / userland convention).
- **`rfcs/`** moved to **`docs/rfcs/`**. Doc-generation in `modules/rust-packages.nix` reads from the new location.
- **`flake.lib`** is now the wired entry; consumers that previously read `inputs.nixfleet.scopes.X` from `nixfleet-scopes` now read `inputs.nixfleet.scopes.X` from this repo (same attribute path, different source).

#### Removed (public surface)

- **`flake.diskoTemplates.*`** — disk templates dropped from public output. `nixfleet`'s QEMU test fixture keeps a co-located template at `tests/fixtures/qemu/disk-template.nix`. Consuming fleets carry their own templates.
- **`flakeModules.{iso, formatter, apps, tests}`** — fleet repos that imported the framework's iso / formatter / apps / tests perSystem modules now host their own.
- **`modules/iso.nix`** and **`modules/formatter.nix`** — consumers absorb these locally.
- **`modules/_hardware/qemu/`** — moved to `tests/fixtures/qemu/` (clearly scoped to framework-internal test harness, not a public output).

#### Earlier in the cycle (still under [Unreleased] from before this refactor)

- `lib.mkFleet` — evaluates a declarative fleet description per RFC-0001 and emits a typed `.resolved` artifact. Every invariant from §4.2 is enforced at eval time: host/channel/policy references, host `configuration` validity, edge DAG, compliance-framework allow-list, and the cross-field `freshnessWindow ≥ 2 × signingIntervalMinutes` relation.
- `lib.withSignature` — helper that CI calls to stamp `meta.signedAt` / `meta.ciCommit` onto a resolved fleet before signing.
- `nixfleet.trust.*` option tree (now at `contracts/trust.nix`) — declares CI release key, attic cache key, and org root key (with rotation grace slots and a compromise `rejectBefore` switch) per `docs/CONTRACTS.md §II`.
- `tests/lib/mk-fleet/` (renamed from `tests/lib/mkFleet/`) — eval-only harness with positive fixtures (golden JSON comparison), negative fixtures (expected-failure via `tryEval`), and `_`-prefix filter for shared helpers.
- New channel options: `signingIntervalMinutes` (default 60) and `freshnessWindow` (no default — must declare). Existing channel definitions must add these to evaluate.
- New host option: `pubkey` (nullable, OpenSSH-format ed25519). Host entries may still omit it; enrollment-bound hosts MUST set it.
- `fleet.resolved` shape extended with a `meta` attribute (`{schemaVersion, signedAt, ciCommit}`) per `docs/CONTRACTS.md §I #1`. Top-level `schemaVersion: 1` is preserved for RFC-0001 §4.1 backward reference.

## [0.1.0] - 2026-04-19

Initial release.

[Unreleased]: https://github.com/arcanesys/nixfleet/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/arcanesys/nixfleet/releases/tag/v0.1.0
