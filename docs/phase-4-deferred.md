# Phase 4 deferred items inventory

Sibling to `docs/phase-4-entry-spec.md`. Captures everything carved out of Phase 3 + Phase 4 implementation, with rationale, cost, risk, and target phase. Intent: a single place an operator can scan to know "what does v0.2 still owe me?".

Last updated: 2026-04-27 (post-lab-validation + GitOps closure + Phase 4 polish).

## Status — Phase 3/4 against the v0.2 brief

Phase 3 wire protocol (RFC-0003) and Phase 4 dispatch+activation are both **proven on real hardware** during the lab validation pass:

- mTLS handshake, `/healthz`, `/whoami`, `/v1/agent/checkin`, `/v1/agent/confirm`, `/v1/agent/report`, `/v1/enroll`, `/v1/agent/renew`, `/v1/agent/closure/<hash>` all live.
- SQLite migrations apply cleanly; `pending_confirms` round-trip verified.
- Reconcile loop primes `verified_fleet` snapshot at boot and every tick.
- **Forgejo poll closes the GitOps loop** — operator pushes to fleet/main → CI re-signs → poll (≤60s) refreshes the snapshot from Forgejo bytes → next checkin dispatches against fresh closureHashes. No CP redeploy required.
- **Dispatch decision per-checkin** issues real targets that drive real activations end-to-end on lab.
- **Magic rollback timer** flips past-deadline rows to `rolled-back` (after the datetime-comparison fix).
- **Agent activation chain**: `nix-store --realise` → `nix-env --profile --set` → `<store-path>/bin/switch-to-configuration switch` → post-switch basename verify → `/v1/agent/confirm`. Bypasses `nixos-rebuild` entirely (stable contract across NixOS releases).
- **`/v1/agent/report`** wire shape locked to RFC §4.3 + operational fields (`agentVersion`, `occurredAt`). Agent now emits typed events (`activation-failed`, `realise-failed`, `verify-mismatch`, `rollback-triggered`, `renewal-failed`, `other`) at every failure path.

112+ tests workspace-wide, all green. Real-hardware validation pass caught and fixed five bugs that integration tests had missed (datetime-string-compare, ECDSA high-s rejection, nixos-rebuild-ng UX rename, Forgejo poll didn't refresh `verified_fleet`, agent missing `nixos-rebuild` on PATH — last superseded by switching to `switch-to-configuration` directly).

## Critical-path

None. The v0.2 dispatch+activation chain is functionally complete.

## Phase 5+ follow-ups (not blocking v0.2 functional completion)

| # | Item | Why deferred | Cost | Risk |
|---|---|---|---|---|
| 1 | Reconciler state-machine extensions: `WaveSoaking` → `WavePromoted` → `onHealthFailure` (RFC-0002 §4.4-§4.6) | Significant reconciler refactor; deserves its own spec. Per-host dispatch already works; this layers wave/soak gates *in front* of the existing decide_target. | ~600 LOC, 3-5 days | Medium — needed for production multi-host coordination beyond the homelab's 5 hosts |
| 2 | Active rollouts table in DB. Currently `pending_confirms` tracks per-host activation; no rollout-lifecycle row. `Observed.active_rollouts` is empty, so reconciler's `OpenRollout`/`HaltRollout`/`PromoteWave`/`ConvergeRollout` actions are emitted into the journal but otherwise unused. | Same scope as #1 (waves need this state). | ~200 LOC + migration | Medium — same as #1 |
| 3 | Disruption budgets enforcement (RFC-0002 §4.2) | Depends on the reconciler state machine | ~200 LOC | Low for 5-host fleet |
| 4 | Edge ordering enforcement (RFC-0002 §4.1) | Depends on reconciler state machine | ~150 LOC | Low (homelab declares no edges) |
| 5 | Real Nix-cache-protocol forwarding in closure proxy (replaces narinfo-only) | Complex (binary cache semantics + signed-narinfo verify) | ~250 LOC, 1-2 days | Low — fallback path, primary is direct attic |

## Wire-shape decisions still to lock

| # | Item | Status |
|---|---|---|
| 6 | `health` + `lastProbeResults` fields on `CheckinRequest` (RFC §4.1) | Phase 7 territory (probes generate the data) |
| 7 | Per-host `nextCheckinSecs` shaping (RFC §5) | Cosmetic at 5 hosts |

## Architectural / sovereignty (Phase 6+ — Tier 2/3 in operator parlance)

These are documented as issues. The v0.2 brief's "control plane holds no secrets, forges no trust" hinges on these.

| # | Item | Issue | Why deferred | Cost | Risk during tech-debt window |
|---|---|---|---|---|---|
| 8 | TPM-bound issuance CA + offline fleet root + name constraints | [#41](https://github.com/abstracts33d/nixfleet/issues/41) | Substrate exists in `nixfleet.tpmKeyslot` scope (already used for CI signing); ~5-8 days to wire end-to-end for the fleet CA path | Phase 7-9 polish | Medium — Tailscale-only access + 5-host fleet bound the blast radius. Single biggest violation of the slogan today. |
| 9 | Host-key-derived agent identity (CSR signing key = SSH host key, not fresh keypair) | [#43](https://github.com/abstracts33d/nixfleet/issues/43) | Mid-complexity refactor; doesn't change wire format | ~200-300 LOC, Phase 6 | Medium — sovereignty property weakened: cert/host-key compromise no longer equivalent |
| 10 | Probe execution + signed evidence (RFC-0003 §7.3) | — (Phase 7 milestone) | Whole separate phase | weeks | Low (compliance not yet a deploy gate) |
| 11 | Compliance gates as rollout blockers | [#4](https://github.com/abstracts33d/nixfleet/issues/4) | Depends on probe execution | ~3 days | Low |

## Documentation

| # | Item | Status |
|---|---|---|
| 12 | `ARCHITECTURE.md` updates reflecting Phase 4 reality (DB layer, dispatch flow, GitOps closure, agent activation via switch-to-configuration) | Pending |
| 13 | `docs/operator-cookbook.md` (deploy, revoke, monitor rollouts, rotate org root, redeploy lab from cold start) | Pending |
| 14 | CHANGELOG / v0.2 release notes | Premature until v0.2 is ready to tag |
| 15 | RFC-0003 §4.3 alignment doc — the shipped `event`/`details` shape now matches the RFC; document the operational additions (`agentVersion`, `occurredAt`, `rollout`) | Pending |

## Operational

| # | Item | Issue | Status |
|---|---|---|---|
| 16 | microvm harness extensions for new Phase 3/4 endpoints | [#5](https://github.com/abstracts33d/nixfleet/issues/5), [#27](https://github.com/abstracts33d/nixfleet/issues/27) | Phase 5 (basic harness already partial) |
| 17 | Phase-10 teardown test ("rebuild CP from empty state") | [#14](https://github.com/abstracts33d/nixfleet/issues/14) | Phase 10 — final v0.2 acceptance gate |
| 18 | Operator CLI commands: `nixfleet revoke`, `nixfleet pending-confirms`, `nixfleet prune-replay` | — | Phase 9 polish |
| 19 | `nixfleet diff` (declared vs observed) | [#8](https://github.com/abstracts33d/nixfleet/issues/8) | Phase 9 |
| 20 | deploy-rs schema compatibility layer | [#7](https://github.com/abstracts33d/nixfleet/issues/7) | Niche — only when migration is real |
| 21 | Persist `host_reports` in DB (currently in-memory ring, capped at REPORT_RING_CAP=32). Now that the agent emits typed events, persistence is worth doing — operators want to query "all reports for rollout X" historically. | — | Phase 5 — small (~80 LOC + migration), waits for the active-rollouts table |

## Polish / cleanup

| # | Item | Where |
|---|---|---|
| 22 | Replace inline PEM parser with `pem` crate | `crates/nixfleet-agent/src/enrollment.rs` |
| 23 | Replace heuristic PKCS#8 parsing with proper parser | `crates/nixfleet-cli/src/bin/mint_token.rs` |
| 24 | Cargo.lock churn / dep audit | Workspace |

## Real-hardware bugs caught + fixed during the lab validation pass

The lab redeploy cycle on 2026-04-27 surfaced bugs that integration tests had missed. Each landed with a regression test:

| Commit | Bug | Why integration tests missed it |
|---|---|---|
| `1d570db` | Magic rollback timer never matched past-deadline rows. `confirm_deadline` stored as RFC3339 (`T`-separator) but `datetime('now')` returns space-separated; lex compare put deadlines forever-greater-than-now. | Tests stored deadlines through the same code path; the SQLite-format mismatch only manifests when comparing to `datetime('now')` runtime. |
| `0821ae9` | Verifier rejected ~50% of TPM-emitted ECDSA signatures (Bitcoin-style strict-low-s). TPM2_Sign produces high-s sigs about half the time. | Test sigs were generated with a Rust ed25519 helper that happens to emit low-s. |
| `6d7b367` | Agent's `nixos-rebuild switch --system <path>` failed under nixos-rebuild-ng (NixOS 26.05 Python rewrite). Renamed flag + nixos-rebuild-ng's `--rollback` evaluates `<nixpkgs/nixos>`, fails in the agent's NIX_PATH-less sandbox. | Agent integration tests don't run real `nixos-rebuild`. |
| `c30e2fe` | Forgejo poll never refreshed `verified_fleet` — only `channel_refs`. CP was effectively stuck on its deploy-time artifact bytes; commits couldn't propagate without a redeploy. The TODO note in `forgejo_poll.rs` had explicitly deferred this as "PR-4.5". | No test exercised commit→activate without redeploy. |
| (in this polish commit) | `/v1/agent/renew` had zero integration coverage; `/enroll` had three. Renewal regressions (mTLS gate, cert-revocation gate, CA-not-configured 500) would slip through. | Test gap; now closed with 4 renew tests. |

## v0.2 issue tracker (#10) — current status

| Issue | Title | Status |
|---|---|---|
| [#1](https://github.com/abstracts33d/nixfleet/issues/1) | fleet.nix schema | ✅ Phase 1 |
| [#2](https://github.com/abstracts33d/nixfleet/issues/2) | Magic rollback in agent | ✅ — local rollback (Phase 4 PR-D), CP detects deadline expiry (Phase 4 PR-B + datetime fix), agent reacts to `/confirm` 410, agent rolls back on post-switch closure-hash mismatch, agent emits `rollback-triggered` reports. End-to-end deadline-expiry path still unexercised on hardware (would need to artificially block the agent's `/confirm` POST). |
| [#3](https://github.com/abstracts33d/nixfleet/issues/3) | GitOps release binding | ✅ — Forgejo poll refreshes verified_fleet from operator commits (`c30e2fe`) |
| [#4](https://github.com/abstracts33d/nixfleet/issues/4) | Compliance as rollout gate | ❌ Phase 6/7 |
| [#5](https://github.com/abstracts33d/nixfleet/issues/5) | microvm harness | 🟡 partial — basic harness exists; not extended for new endpoints |
| [#6](https://github.com/abstracts33d/nixfleet/issues/6) | agenix secrets, no cleartext on CP | 🟡 mostly done — fleet CA private key online is the remaining cleartext (#41) |
| [#7](https://github.com/abstracts33d/nixfleet/issues/7) | deploy-rs compat | ❌ |
| [#8](https://github.com/abstracts33d/nixfleet/issues/8) | `nixfleet diff` | ❌ |
| [#9](https://github.com/abstracts33d/nixfleet/issues/9) | Declarative enrollment | 🟡 mostly there — bootstrap tokens via fleet-secrets work; agenix entry now conditional on token file existence so hosts with pre-issued certs don't need one |
| [#12](https://github.com/abstracts33d/nixfleet/issues/12) | Signed artifacts | 🟡 2/3 done — CI release key (Phase 1) + attic cache key (Phase 1); host probe signatures Phase 7 |
| [#13](https://github.com/abstracts33d/nixfleet/issues/13) | Freshness window | ✅ implemented in `verify_artifact` |
| [#14](https://github.com/abstracts33d/nixfleet/issues/14) | Phase-10 teardown test | ❌ — final acceptance gate |
| [#41](https://github.com/abstracts33d/nixfleet/issues/41) | TPM-bound issuance CA | ❌ Phase 7-9 — single biggest sovereignty gap |
| [#43](https://github.com/abstracts33d/nixfleet/issues/43) | Host-key-derived identity | ❌ Phase 6 |

## Honest summary

**v0.2 functional completion**: Phase 3 wire + Phase 4 dispatch+activation are both functionally complete and proven on lab hardware. GitOps loop closed. The activation chain runs end-to-end (`commit → CI → poll → dispatch → realise → switch-to-configuration → confirm → DB row marked confirmed`) in ~3 seconds on real hardware.

**Most impactful remaining sovereignty gap**: [#41](https://github.com/abstracts33d/nixfleet/issues/41) (TPM-bound CA). Wire works; "CP holds no secrets" is broken in steady-state — the fleet CA private key is agenix-decrypted on lab and read at issuance time.

**Most impactful remaining functionality gap**: reconciler state-machine extensions (#1 above) for multi-wave / soak / health-gate enforcement. Per-host dispatch is unconditional today; production multi-host coordination needs the wave-soak-promote sequencing the reconciler already emits actions for but the CP doesn't act on.

**Recommended next-session order**:

1. **Tag `v0.2.0-rc1`** — Phase 3/4 surface is stable; rc-tag captures the milestone.
2. **Tier 2: TPM-bound CA (#41)** — biggest sovereignty win, ~1 week. Substrate already exists for CI signing; extend to fleet-CA issuance.
3. **Tier 3: host-key-derived identity (#43)** — ~3 days, mid-complexity refactor. Pairs naturally with #41.
4. **Phase 5: reconciler state machine + active-rollouts table** — production-grade multi-host coordination. ~1-2 weeks.
5. **Phase 7: probe execution + signed evidence + compliance gates** — separate phase.
6. **Phase 10: teardown test** — final v0.2 acceptance.
