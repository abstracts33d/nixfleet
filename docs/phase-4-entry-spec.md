# Phase 4 entry spec

Sequences the activation layer onto the wire protocol Phase 3 shipped. Where Phase 3 turned the CP into a long-running TLS server with mTLS-authenticated agents that *could* exchange state but never act on it, Phase 4 makes the agents act: real `nixos-rebuild switch` execution, deadline-tracked confirmation, and magic rollback when activation fails. By the end of Phase 4:

- Lab's CP dispatches targets to agents in `/v1/agent/checkin` responses, deriving them from the reconciler's plan.
- Each fleet host runs `nixos-rebuild switch` when the CP hands it a non-null `target`, then posts `/v1/agent/confirm` to acknowledge.
- The CP records every dispatch as a `pending_confirms` row with a deadline. Missed confirms get marked `rolled-back` by a periodic timer and the agent is signalled to revert.
- `/v1/agent/closure/<hash>` proxies the binary cache for hosts that can't reach attic directly.

**Not in Phase 4** (deferred to Phase 5+):
- Wave / soak / promote state machine (RFC-0002 §4.4-§4.6) — dispatch is per-host today; channel-level rollout staging lands later.
- Disruption budgets enforcement (RFC-0002 §4.2).
- Edge ordering enforcement (RFC-0002 §4.1).
- Probe execution + signed evidence (Phase 7).
- Compliance gates as rollout blockers (Phase 6/7).
- TPM-bound issuance CA (issue #41 — Phase 7-9 polish).
- Host-key-derived agent identity (issue #43 — Phase 6 candidate).

Cross-references: `docs/KICKOFF.md §1 Phase 4`, `rfcs/0002-reconciler.md §4` (decision procedure), `rfcs/0003-protocol.md §4.2` (`/confirm` semantics).

Status: **retroactive — most of the chunks below already shipped on `phase-4-rolled-up`**. Written after the fact at the operator's request to formalise what was sequenced ad-hoc. Section labels match what was actually implemented; deviations from this plan are noted inline.

## 1. Architectural shift

Phase 3 was:

```
agent → /v1/agent/checkin (every 60s)
                          ↓
                          CP records, returns target=null forever
                          (Phase 3 never dispatches)
```

Phase 4 becomes:

```
agent → /v1/agent/checkin (every 60s)
                          ↓
                          dispatch loop reads pending rollouts,
                          populates target=<closure_hash, channel_ref>
                          ↓
                          CP records `pending_confirms` row with
                          confirm_deadline = now + 120s
                          ↓
                          response: target = {…}, nextCheckinSecs = 60
                          ↓
agent runs nixos-rebuild switch
        ↓ success
        agent → /v1/agent/confirm (rollout, wave, generation)
                                 ↓
                                 CP marks pending_confirms.confirmed_at,
                                 state='confirmed'
                                 ↓
                                 response: 204 No Content

        ↓ failure (rebuild non-zero exit OR deadline missed)
        local rollback (nixos-rebuild --rollback) AND
        either (a) /v1/agent/confirm 410'd because deadline passed,
        or (b) the rollback timer marked the row state='rolled-back'

        Either way, next checkin response: target=null again
        until reconciler decides what to do next.
```

The shift is real: the reconciler's *plan* becomes the CP's *intent broadcast to agents*. PR-1 below introduces the persistence layer that makes intent durable across CP restarts; subsequent PRs fill in the dispatch + confirm + rollback paths.

## 2. PR breakdown

### PR-1 — SQLite foundation (✅ shipped)

**Scope.** Stand up the persistence layer Phase 4 needs for cross-restart state. Three tables: `token_replay` (promote Phase 3's in-memory HashSet), `cert_revocations` (RFC-0003 §2 — Phase 3 didn't ship the data structure), `pending_confirms` (the activation deadline tracker).

**Concrete.**

- `crates/nixfleet-control-plane/Cargo.toml`: add `rusqlite 0.32` (bundled feature) and `refinery 0.8`.
- `migrations/V002__phase4_init.sql` — three tables with appropriate indexes. V001 is the PR #29 placeholder (kept for refinery history compat).
- `src/db.rs` — `Db` struct with `Mutex<Connection>`, WAL + FK pragmas at startup, accessors for all three tables. Skeleton ported from v0.1's CP (tag v0.1.1).
- `AppState` gains `db: Option<Arc<Db>>`. `None` keeps the in-memory file-backed deploy path working for tests.
- `--db-path` / `NIXFLEET_CP_DB_PATH` flag. Default `/var/lib/nixfleet-cp/state.db` lives under `StateDirectory`.
- NixOS module: `services.nixfleet-control-plane.dbPath` option (nullable; null disables persistence).

**Tests.** 3 unit tests in `src/db.rs`: schema-creation, token-replay round-trip, cert-revocation upsert.

**Status.** Live on phase-4-rolled-up.

### PR-A — `/v1/agent/confirm` real wire (✅ shipped)

**Scope.** The CP-side handler that the agent calls after a successful `nixos-rebuild switch`. Marks the matching `pending_confirms` row as confirmed; responds 410 Gone when no matching pending row exists (rollout cancelled, deadline already passed, agent confused).

**Concrete.**

- `nixfleet_proto::agent_wire::ConfirmRequest` / `ConfirmResponse`.
- `db::record_pending_confirm` (called by dispatch loop — TODO) and `db::confirm_pending` (called by handler).
- Handler validates body.hostname matches verified mTLS CN (403 on mismatch). 503 if no DB. 204 on success.

**Tests.** None yet — handler is unreachable until dispatch loop creates `pending_confirms` rows. Test backfill is a follow-up.

**Status.** Live on phase-4-rolled-up.

### PR-B — Magic rollback timer (✅ shipped)

**Scope.** Periodic background task, every 30s scans `pending_confirms` for rows whose `confirm_deadline` has passed but `state` is still `'pending'`. Transitions each to `'rolled-back'` and emits a journal line per host.

**Concrete.**

- `db::pending_confirms_expired` returns `Vec<(id, hostname, rollout_id, wave, target_closure_hash)>`.
- `db::mark_rolled_back(&[id])` — idempotent UPDATE WHERE state='pending'.
- `src/rollback_timer.rs` — `tokio::time::interval(30s)` loop. `MissedTickBehavior::Skip`.
- Spawned in `serve()` only when `state.db` is set.

**Tests.** Unit test: empty table → empty result. End-to-end roundtrip test deferred until PR-A's `record_pending_confirm` accessor is wired into a publicly-accessible test path.

**Status.** Live on phase-4-rolled-up.

### PR-C — Closure proxy with attic-narinfo upstream (✅ shipped)

**Scope.** Replaces the `feat/phase-4-closure-proxy` skeleton (501 stub) with real attic narinfo forwarding. Full Nix-cache-protocol forwarding (nar streaming) is a follow-up.

**Concrete.**

- `ServeArgs` gains `closure_upstream: Option<String>`. `AppState` gains `closure_upstream: Option<ClosureUpstream>` (URL + pre-built reqwest client).
- `closure_proxy` handler: forwards `GET <upstream>/<hash>.narinfo`, returns upstream status + body verbatim. 502 on upstream unreachable, 501 if upstream unset.
- `--closure-upstream` / `NIXFLEET_CP_CLOSURE_UPSTREAM` flag. NixOS option `services.nixfleet-control-plane.closureUpstream`.

**Tests.** None — would need a stub attic. Deferred until full nar forwarding lands.

**Status.** Live on phase-4-rolled-up. Real nar forwarding deferred (follow-up tracked in the deferred-items analysis).

### PR-D — Agent activation loop + local rollback (✅ shipped)

**Scope.** When the CP returns a non-null `target` in CheckinResponse, the agent runs `nixos-rebuild switch`. On rebuild failure, runs `nixos-rebuild --rollback`. Posts `/v1/agent/confirm` after success.

**Concrete.**

- `src/activation.rs` — `activate(target)`, `rollback()`, `confirm_target(target)`.
- Activation hook in `src/main.rs`'s poll loop.
- `confirm_target()` posts `ConfirmRequest` to `/v1/agent/confirm` (real implementation, not a stub — see §3 below).
- `closure_hash` → `/nix/store/<hash>` resolution: assumes the closure_hash is the full store-path basename. TODO if the CP starts sending bare hashes.

**Tests.** None — would need a stub `nixos-rebuild`. Deferred to microvm harness.

**Status.** Live on phase-4-rolled-up.

### Parallel children (✅ shipped, not strictly Phase 4)

- **token-replay-db**: promotes the Phase 3 in-memory HashSet to SQLite when configured. In-memory fallback when DB unset. Same observable semantics either way.
- **cert-revocation**: enforces revocation in `require_cn` (mTLS auth path). `cert.notBefore < cert_revocations.not_before` → 401.
- **agent-poll-backoff** (RFC-0003 §5 fix): exponential backoff with ±20% jitter on consecutive checkin failures. Capped at 8× base interval.
- **protocol-version-header** (RFC-0003 §6 fix): `X-Nixfleet-Protocol: 1` header on every /v1/* request. CP middleware enforces; missing accepted (forward-compat) for older agents during transition.

## 3. What remains in Phase 4 (NOT yet shipped)

### Dispatch loop (critical path)

**Scope.** Populate `CheckinResponse.target` from reconciler decisions. Without this, the entire activation chain is unreachable: agents get `target: null` on every checkin, never run `nixos-rebuild`, no `pending_confirms` rows ever exist, magic-rollback timer scans an empty table forever.

**Concrete (sketched).**

- New `src/dispatch.rs`:
  - `decide_target(host: &str, fleet: &FleetResolved, observed: &Observed) -> Option<EvaluatedTarget>` — pure function that, given a host's current closure and the channel's target ref, returns the closure the host *should* be running (or None if already converged).
  - The reconciler's existing `Action::OpenRollout` events feed this — when a rollout opens, the dispatch fn picks targets per-host based on wave membership.
- `checkin` handler in `server.rs`: after recording the checkin, look up the host's dispatch decision. If non-null, write a `pending_confirms` row and include the target in the response.
- Confirm-deadline default: 120s (RFC-0003 §4.1 example value).

**Estimate.** 200-300 LOC + tests. Mid-sized PR. Should land before any production deploy.

### `/v1/agent/confirm` agent-side POST (small finishing touch — already in this session)

**Status.** Originally a TODO stub on PR-D; **wired up in this session** (see §4 below).

### Reconciler state-machine extensions

**Scope.** Bring the reconciler from "dispatch happens" to "dispatch happens with proper safety": waves, soak window, health-gate failure handling, disruption budgets across rollouts, edge ordering within a wave.

**Concrete (sketched).**

- `nixfleet-reconciler` crate: extend `Action` enum with `WaveSoaking`, `WavePromoted`, `RolloutHalted`.
- `Observed.host_state[host]` gains states from RFC-0002 §3.2: `Queued | Dispatched | ConfirmWindow | Healthy | Soaked | Failed | Reverted`.
- `reconcile()` updated to honour budget + edge constraints when emitting actions.
- CP's `pending_confirms` table extended with state machine columns; migration V003 adds them.

**Estimate.** Substantial — likely 5+ days, deserves its own spec doc and PR chain.

### Real Nix-cache-protocol forwarding in closure proxy

**Scope.** Replace narinfo-only forwarding with full nar streaming. Multi-file closure transfer; signed-narinfo verification before forwarding (so the CP doesn't proxy something attic claims is signed but isn't).

**Concrete.**

- Stream response from upstream attic to the agent without buffering (nars can be large).
- Verify the closure's signed narinfo against the cache-signing-key trust root before serving.
- Route handlers for `/v1/agent/cache/<hash>.narinfo` and `/v1/agent/cache/nar/<hash>.nar.zst`.

**Estimate.** Medium — 200-300 LOC. Optional in v0.2 (the primary path is direct attic; the proxy is a fallback).

## 4. What this commit adds (the "fold in" work)

Beyond the previously-rolled-up state, this commit on `phase-4-rolled-up` adds two things:

### 4.1 This spec document

`docs/phase-4-entry-spec.md` (the file you're reading). Retroactive formalisation of Phase 4's plan, mirroring the format of `docs/phase-3-entry-spec.md`. Lets the next session start from a written plan rather than chat-thread reconstruction.

### 4.2 Agent's `/v1/agent/confirm` POST — real wire

PR-D shipped `agent::activation::confirm_target()` as a TODO stub that logged but didn't post. With PR-A's `ConfirmRequest` wire types now in this same branch (rolled-up state), there's no reason to leave the stub.

The real body:

- Build a `ConfirmRequest` from the activation's target + the agent's `currentGeneration` (closure hash, boot ID).
- POST to `/v1/agent/confirm` over the existing mTLS reqwest client.
- Map response codes:
  - **204 No Content** → ok, activation acknowledged.
  - **410 Gone** → CP says rollout was cancelled or deadline passed. Agent triggers local rollback.
  - **other 4xx/5xx** → log + treat as activation-not-acknowledged (deadline timer will fire; agent doesn't need to do anything special).

This closes the `nixos-rebuild → /confirm → CP records → magic-rollback timer` loop. With dispatch loop also landed (Phase 4 follow-up), Phase 4 is functionally complete for single-channel rollouts.

## 5. Cargo dep changes per PR

| PR | Adds to nixfleet-control-plane | Adds to nixfleet-agent | Adds to nixfleet-proto |
|---|---|---|---|
| PR-1 | rusqlite, refinery | — | — |
| PR-A | — | — | (no deps; types only) |
| PR-B | — (uses rusqlite from PR-1) | — | — |
| PR-C | — (uses reqwest from PR-4) | — | — |
| PR-D | — | rcgen, sha2, base64, x509-parser (already from PR-5), rand (backoff fix) | — |

## 6. Order

The Phase 4 work landed in roughly this order on phase-4-rolled-up:

1. PR-1 (SQLite foundation)
2. PR-A, PR-B, PR-C, PR-D (parallel — branched off post-PR-1, merged in any order)
3. token-replay-db, cert-revocation (parallel children)
4. agent-poll-backoff, protocol-version-header (RFC fixes)

Dispatch loop is sequenced **after** all of the above and is the next critical-path PR. Reconciler state-machine extensions sit on top of dispatch.

Rough size estimates of the deferred remainder:

| Chunk | Rust LOC | Effort (focused) |
|---|---|---|
| Dispatch loop | ~250 | 1-2 days |
| Reconciler state machine | ~600 | 3-5 days |
| Real closure forwarding | ~200 | 1-2 days |
| Test backfill (Phase 4 endpoints) | ~600 | 1-2 days |

Phase 4 to "complete" status: **~1 week focused / 2-3 weeks part-time**.

## 7. Decisions to lock in before dispatch loop

### D1 — `confirmWindowSecs` default

**Default.** 120s (RFC-0003 §4.1 example). Long enough for an agent's `nixos-rebuild switch` (typically 30-60s on a workstation, longer for full closures with nix-copy) plus reboot if needed (servers ~1min, workstations ~10s if not switching kernel). Tight enough that operator sees rollback within ~2min of a bad activation.

**Alternatives.** 60s (assumes warm cache + no kernel switch — risky for cold-cache deployments). 300s (very conservative, slow rollback feedback).

### D2 — Per-host vs per-rollout target tracking

**Default.** Per-host. `pending_confirms` rows are per (hostname, rollout_id) tuple; a host can have at most one pending row per rollout. If a new rollout opens before the host confirms the previous, the older row gets cancelled (state='cancelled') and the new one takes over.

**Alternatives.** Allow multiple in-flight rollouts per host (queued by deadline). Adds complexity for marginal benefit — skip.

### D3 — Dispatch loop trigger

**Default.** Inline in `/v1/agent/checkin` handler. When a checkin arrives, the handler checks the dispatch decision and inserts the row + populates the response.

**Alternatives.** Background task that pre-computes targets and stores them. More complex; deferred.

## 8. Test substrate

Phase 4 tests should extend the harness from PR #34. Specifically:

- **dispatch test**: agent checks in, CP populates target, agent activates (stubbed), agent confirms, CP marks confirmed.
- **rollback test**: agent checks in, CP populates target, agent activation fails (stubbed nixos-rebuild returns non-zero), agent local-rollback succeeds, no /confirm posted, deadline expires, magic-rollback timer marks row 'rolled-back'.
- **cert revocation test**: revoke a host via direct DB INSERT, agent's next mTLS handshake gets rejected.
- **closure proxy test**: stub attic returns narinfo, CP forwards, agent reads.

Phase 4's overall test coverage should hit ~40 integration tests (Phase 3 had 22; +20 across the new endpoints + magic-rollback round-trip).

## 9. Cross-references to deferred items

- `docs/phase-4-deferred.md` (sibling to this file) — full inventory of what's deferred from Phase 4 + later phases, with rationale and risk assessments.
- `nixfleet#41` — TPM-bound issuance CA (Phase 7-9 polish).
- `nixfleet#43` — Host-key-derived agent identity (Phase 6 candidate).
- `nixfleet#10` — v0.2 tracking issue.

## 10. When dispatch loop lands, deploy substrate

After dispatch is in:

1. Operator commits a no-op release commit. CI signs.
2. CI's commit hook updates `releases/fleet.resolved.json` in the fleet repo. Lab's CP's Forgejo poll picks it up within 60s.
3. Reconciler tick sees the new channel ref, emits `OpenRollout`.
4. Next checkin from each fleet host: CP returns target = (new closure hash, channel ref).
5. Agent runs `nixos-rebuild switch`. On success, posts /confirm. CP marks confirmed.
6. Operator sees the rollout converge in `journalctl -u nixfleet-control-plane | grep '"event":"tick"'`.

That's the v0.2 deliverable. Everything past that — wave staging, disruption budgets, signed evidence, TPM-bound CA — is Phase 5+ polish.
