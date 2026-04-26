# Phase 4 deferred items inventory

Sibling to `docs/phase-4-entry-spec.md`. Captures everything carved out of Phase 3 + early Phase 4 implementation, with rationale, cost, risk, and target phase. Intent: a single place an operator can scan to know "what does v0.2 still owe me?".

Last updated: 2026-04-26 (post-`phase-4-dispatch`).

## Critical-path (blocks v0.2 functional completion)

| # | Item | Why deferred | Cost | Risk |
|---|---|---|---|---|
| 1 | ~~Dispatch loop~~ — ✅ landed on `phase-4-dispatch`. CP snapshots verified `FleetResolved` each tick + at boot; `/v1/agent/checkin` calls `dispatch::decide_target` against the snapshot, inserts a `pending_confirms` row keyed on `<channel>@<ci-commit-prefix>`, and returns `EvaluatedTarget`. 9 unit tests + 3 integration tests cover the decision matrix (Unmanaged / NoDeclaration / Converged / InFlight / HoldAfterFailure / Dispatch) and round-trip a real ed25519-signed `fleet.resolved` through verify → snapshot → checkin → DB. | — | — |
| 2 | **End-to-end deployment validation on lab** | Code shipped; never deployed | 0 LOC, ~1 hour wall clock | **HIGH** — uncovered failure modes; "compiles" ≠ "works" |

## Phase 4 follow-ups (next sprint, sequenced)

| # | Item | Why deferred | Cost | Risk |
|---|---|---|---|---|
| 3 | Reconciler state-machine extensions: `WaveSoaking` → `WavePromoted` → `onHealthFailure` (RFC-0002 §4.4-§4.6) | Significant reconciler refactor; deserves its own spec | ~600 LOC, 3-5 days | Medium — needed for production multi-host coordination |
| 4 | Disruption budgets enforcement (RFC-0002 §4.2) | Depends on dispatch loop | ~200 LOC | Low for 5-host fleet |
| 5 | Edge ordering enforcement (RFC-0002 §4.1) | Depends on dispatch loop | ~150 LOC | Low (homelab declares no edges) |
| 6 | Real Nix-cache-protocol forwarding in closure proxy (replaces narinfo-only) | Complex (binary cache semantics + signed-narinfo verify) | ~250 LOC, 1-2 days | Low — fallback path, primary is direct attic |
| 7 | Test coverage backfill: `/renew` (mid-priority — similar shape to `/enroll`), magic-rollback round-trip, dispatch tests | Time | ~400 LOC tests | Medium — without tests, regressions undetected |

## Wire-shape decisions to lock

| # | Item | Decision shape |
|---|---|---|
| 8 | `/v1/agent/report` body shape: align to RFC §4.3 (`event` + structured `details.{phase, exitCode, stderrTail}`) OR amend RFC to match shipped shape (`kind` enum + free-form `error` + `context: Value`) | Decide when dispatch lands and `/report` is exercised with real activation data |
| 9 | `health` + `lastProbeResults` fields on `CheckinRequest` (RFC §4.1) | Phase 7 territory (probes generate the data) |
| 10 | Per-host `nextCheckinSecs` shaping (RFC §5) | Cosmetic at 5 hosts |

## Architectural / sovereignty (Phase 6+)

These are documented as issues. The "spirit of v0.2" (issue #10's "control plane holds no secrets, forges no trust") hinges on these.

| # | Item | Issue | Why deferred | Cost | Risk during tech-debt window |
|---|---|---|---|---|---|
| 11 | TPM-bound issuance CA + offline fleet root + name constraints | [#41](https://github.com/abstracts33d/nixfleet/issues/41) | Substrate exists in `nixfleet.tpmKeyslot` scope; ~5-8 days to wire end-to-end | Phase 7-9 polish | Medium — Tailscale-only access + 5-host fleet bound the blast radius |
| 12 | Host-key-derived agent identity (CSR signing key = SSH host key, not fresh keypair) | [#43](https://github.com/abstracts33d/nixfleet/issues/43) | Mid-complexity refactor; doesn't change wire format | ~200-300 LOC, Phase 6 | Medium — sovereignty property weakened: cert/host-key compromise no longer equivalent |
| 13 | Probe execution + signed evidence (RFC-0003 §7.3) | — (Phase 7 milestone) | Whole separate phase | weeks | Low (compliance not yet a deploy gate) |
| 14 | Compliance gates as rollout blockers | [#4](https://github.com/abstracts33d/nixfleet/issues/4) | Depends on probe execution | ~3 days | Low |

## Documentation

| # | Item | Status |
|---|---|---|
| 15 | `docs/phase-4-entry-spec.md` | ✅ committed (this commit) |
| 16 | `docs/phase-4-deferred.md` | ✅ this file |
| 17 | `ARCHITECTURE.md` updates reflecting Phase 4 reality (DB layer, magic rollback flow, dispatch flow shape) | Pending |
| 18 | `docs/operator-cookbook.md` (deploy, revoke, monitor rollouts, rotate org root) | Pending |
| 19 | CHANGELOG / v0.2 release notes | Premature until v0.2 is ready to tag |

## Operational

| # | Item | Issue | Status |
|---|---|---|---|
| 20 | microvm harness extensions for new Phase 3/4 endpoints | [#5](https://github.com/abstracts33d/nixfleet/issues/5), [#27](https://github.com/abstracts33d/nixfleet/issues/27) | Phase 5 (basic harness already partial) |
| 21 | Phase-10 teardown test ("rebuild CP from empty state") | [#14](https://github.com/abstracts33d/nixfleet/issues/14) | Phase 10 — final v0.2 acceptance |
| 22 | Operator CLI commands: `nixfleet revoke`, `nixfleet pending-confirms`, `nixfleet prune-replay` | — | Phase 9 polish |
| 23 | `nixfleet diff` (declared vs observed) | [#8](https://github.com/abstracts33d/nixfleet/issues/8) | Phase 9 |
| 24 | deploy-rs schema compatibility layer | [#7](https://github.com/abstracts33d/nixfleet/issues/7) | Niche — only when migration is real |

## Polish / cleanup

| # | Item | Where |
|---|---|---|
| 25 | Refactor `forgejo_poll`'s mirror task → shared `Arc<RwLock<>>` | `server.rs::serve` (TODO marked) |
| 26 | Replace inline PEM parser with `pem` crate | `crates/nixfleet-agent/src/enrollment.rs` |
| 27 | Replace heuristic PKCS#8 parsing with proper parser | `crates/nixfleet-cli/src/bin/mint_token.rs` |
| 28 | ~~`closure_hash` → store-path resolution: agent currently assumes the hash IS the basename~~ — partially closed by `phase-4-dispatch`: agent now `nix-store --realise`s the path before switch (catches missing/corrupted closures + substituter-trust failures) and verifies `/run/current-system` basename matches `target.closure_hash` after switch (catches "switched to a different path"). Mismatch triggers local rollback. The basename-vs-bare-hash assumption itself remains — a follow-up CP endpoint to look up store paths from raw hashes is still nice-to-have but not blocking. |
| 29 | Cargo.lock churn / dep audit | Workspace |

## v0.2 issue tracker (#10) — current status

| Issue | Title | Status |
|---|---|---|
| [#1](https://github.com/abstracts33d/nixfleet/issues/1) | fleet.nix schema | ✅ Phase 1 |
| [#2](https://github.com/abstracts33d/nixfleet/issues/2) | Magic rollback in agent | 🟢 mostly done — agent does local rollback (Phase 4 PR-D), CP detects deadline expiry (Phase 4 PR-B), agent reacts to /confirm 410, agent now ALSO rolls back on post-switch closure-hash mismatch (`phase-4-dispatch`). Remaining: end-to-end test on lab/microvm that exercises a real deadline-expiry path |
| [#3](https://github.com/abstracts33d/nixfleet/issues/3) | GitOps release binding | ❌ — channels still operator-imperative |
| [#4](https://github.com/abstracts33d/nixfleet/issues/4) | Compliance as rollout gate | ❌ Phase 6/7 |
| [#5](https://github.com/abstracts33d/nixfleet/issues/5) | microvm harness | 🟡 partial — basic harness exists; not extended for new endpoints |
| [#6](https://github.com/abstracts33d/nixfleet/issues/6) | agenix secrets, no cleartext on CP | 🟡 mostly done — fleet CA private key online is the remaining cleartext (#41) |
| [#7](https://github.com/abstracts33d/nixfleet/issues/7) | deploy-rs compat | ❌ |
| [#8](https://github.com/abstracts33d/nixfleet/issues/8) | `nixfleet diff` | ❌ |
| [#9](https://github.com/abstracts33d/nixfleet/issues/9) | Declarative enrollment | 🟡 mostly there — bootstrap tokens via fleet-secrets work |
| [#12](https://github.com/abstracts33d/nixfleet/issues/12) | Signed artifacts | 🟡 2/3 done — CI release key (Phase 1) + attic cache key (Phase 1); host probe signatures Phase 7 |
| [#13](https://github.com/abstracts33d/nixfleet/issues/13) | Freshness window | ✅ implemented in `verify_artifact` |
| [#14](https://github.com/abstracts33d/nixfleet/issues/14) | Phase-10 teardown test | ❌ — final acceptance gate |
| [#41](https://github.com/abstracts33d/nixfleet/issues/41) | TPM-bound issuance CA | ❌ Phase 7-9 |
| [#43](https://github.com/abstracts33d/nixfleet/issues/43) | Host-key-derived identity | ❌ Phase 6 |

## Honest summary

**Most impactful deferred item:** ~~the dispatch loop~~ end-to-end deployment validation on lab. Dispatch loop landed on `phase-4-dispatch`, with unit + integration tests proving the activation chain reaches `nixos-rebuild` end-to-end. Lab still hasn't seen any Phase 3/4 code; real failure modes (substituter trust on first run, attic narinfo edge cases, the renew loop under cert TTLs) will only surface on first deploy.

**Most impactful deferred sovereignty item:** [#41](https://github.com/abstracts33d/nixfleet/issues/41) (TPM-bound CA). Wire works; "CP holds no secrets" is broken. Acceptable for homelab; blocking for any wider deployment.

**Most impactful deferred quality item:** the lab deploy. See above — this is now both the quality and the next-impactful-work item.

Recommended next-session order:

1. Deploy `phase-3-rolled-up` to lab. Confirm Phase 3 works.
2. Deploy `phase-4-dispatch` to lab with a fresh release commit. Validate full activation loop end-to-end.
3. Tag `v0.2.0-rc1` if the deploy works.
4. Reconciler state machine extensions (waves, soaking, halt) — Phase 5.
5. Sovereignty hardening (#41, #43) — Phase 6+.
