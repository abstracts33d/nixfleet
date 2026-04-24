# feat/3-reconciler-proto — Design Spec

**Date:** 2026-04-24
**Status:** Draft (awaiting user review)
**Primary issue:** abstracts33d/nixfleet#3 (GitOps release binding: channels pinned to git revisions — reconciler wiring)
**Partial-close issues:** #12 (Rust portion — signature verification), #13 (freshness window enforcement)
**Branch:** `feat/3-reconciler-proto` (stacked on `feat/12-canonicalize-jcs-pin` until #16 merges)
**Worktree:** `.worktrees/stream-c-reconciler`
**Stream:** C (Rust). Parallel: Stream B in `.worktrees/mkfleet-promotion` has landed fixtures this PR must match.

## Goals

- Promote the spike reconciler (`spike/reconciler/src/main.rs`, ~200 LOC) to production as two new workspace crates:
  - `crates/nixfleet-proto` — serde types for `docs/CONTRACTS.md §I #1` (`fleet.resolved.json`) only. Matches Stream B's emitted shape byte-for-byte.
  - `crates/nixfleet-reconciler` — modular reconciler with two public functions: `verify_artifact` (RFC-0002 §4 step 0) and `reconcile` (steps 1–6).
- Implement RFC-0002 §4 step 0 (signature verification + freshness check) — closes #13 and the Rust portion of #12.
- Cover every RFC-0002 §3 state-machine transition that is reconciler-driven (both rollout-level §3.1 and per-host §3.2) with fixture-based tests — 16 transitions total. Skip agent-reported intermediate states (Dispatched→Activating→ConfirmWindow are passive for the reconciler) and operator-override states (Cancelled requires CLI work). Skip combinatoric edge cases.
- Validate proto types against Stream B's `*.resolved.json` fixtures on `feat/mkfleet-promotion` before locking them.

## Non-Goals

- **No agent, control-plane, or CLI crate work.** Those are separate PRs (Stream C Milestone 1 is delivered across 3–4 PRs, this is the second after canonicalize).
- **No wire protocol / probe descriptor / probe output types in `nixfleet-proto`.** They land with the agent+CP PR. This PR ships only the fleet.resolved type.
- **No real CI release key integration.** `verify_artifact` takes a `VerifyingKey` parameter; tests generate ed25519 keypairs in-test. Stream A's actual key gets wired when they hand it over (Phase 2 per `docs/KICKOFF.md`).
- **No internal observed-state types in `nixfleet-proto`.** Per `docs/CONTRACTS.md §VI` internal reconciler data structures are non-contracts; they live inside `nixfleet-reconciler` as `src/observed.rs`.
- **No state-machine *execution* (dispatching actions to agents).** The reconciler emits `Vec<Action>`; acting on actions is the CP's job in a later PR.
- **No `docs/CONTRACTS.md` amendment.** This PR does not change contract text — it implements the existing contract. PR #16 already amended §III.
- **No touching `lib/`, `modules/`, `spike/`, `crates/{agent,cli,control-plane,shared,nixfleet-canonicalize}`.** Stream B owns the first three; the v0.1 crates are out of scope until Phase 4 trim; `nixfleet-canonicalize` is the dep, not modified here.
- **No RFC 8785 Appendix E conformance corpus** (deferred from #16).

## Approach

### Two crates, stacked on #16

```
crates/nixfleet-proto/            — boundary-contract types (CONTRACTS.md §I #1 only)
crates/nixfleet-reconciler/       — pure decision procedure + step 0 verification
```

Both are new members of the existing `[workspace]`. Follow the same manifest conventions as `nixfleet-canonicalize` (edition 2021, MIT, explicit `[lib]`).

### Public API surface

Two functions, intentionally decoupled so the CP tick loop composes them and tests exercise each independently:

```rust
// crates/nixfleet-reconciler/src/lib.rs — re-exports from verify.rs and reconcile.rs

pub use verify::{verify_artifact, VerifyError};
pub use reconcile::reconcile;
pub use action::Action;
pub use observed::{Observed, HostState, Rollout};

// verify.rs
pub fn verify_artifact(
    signed_bytes: &[u8],
    signature: &[u8; 64],
    pubkey: &ed25519_dalek::VerifyingKey,
    now: DateTime<Utc>,
    freshness_window: Duration,
) -> Result<FleetResolved, VerifyError>;

// reconcile.rs
pub fn reconcile(
    fleet: &FleetResolved,
    observed: &Observed,
    now: DateTime<Utc>,
) -> Vec<Action>;
```

### Modular internal layout (per approved Approach 2)

```
crates/nixfleet-reconciler/src/
├── lib.rs             — public re-exports only; no logic
├── verify.rs          — verify_artifact + VerifyError
├── reconcile.rs       — reconcile fn: top-level coordinator calling into rollout_state + host_state
├── rollout_state.rs   — rollout-level transitions (RFC-0002 §3.1)
├── host_state.rs      — per-host transitions (RFC-0002 §3.2)
├── budgets.rs         — disruption budget evaluation (§4.2)
├── edges.rs           — edge predecessor ordering (§4.1)
├── observed.rs        — Observed, HostState, Rollout structs (internal, per CONTRACTS §VI)
└── action.rs          — Action enum
```

Every source file is targeted at ≤ 250 LOC. `lib.rs` is a pure re-export shell — no logic there.

### Proto crate — mirror Stream B byte-for-byte

Stream B's `feat/mkfleet-promotion` emits shapes the spike never had. Required proto adaptations, all traced to a specific Stream B commit:

| Field | Where | Source |
|---|---|---|
| `meta` is always present (not optional) | top-level | Stream B commit `8d80361` |
| `meta.{schemaVersion, signedAt, ciCommit}` | `signedAt` + `ciCommit` are `Option<...>` (null when unsigned) | same |
| `hosts.<n>.pubkey: Option<String>` | per-host | same |
| `channels.<n>.freshnessWindow: u32` (minutes) | per-channel | `b29c405` |
| `channels.<n>.signingIntervalMinutes: u32` | per-channel | same |
| `healthGate` may be `{}` — every inner field optional | per-policy | empirical from fixtures |

**Serde posture (decided, no longer open):** `#[serde(default)]` on every `Option<T>` field, WITHOUT `skip_serializing_if`. Matches Stream B's behavior — null round-trips as null, absent round-trips as absent. Canonical bytes are thereby byte-identical to what Stream B emits.

**Roundtrip test:** loads Stream B's real `tests/lib/mkFleet/fixtures/*.resolved.json` files (read via `include_str!` against the committed path on their branch — we read them off our own checkout as part of the merged main eventually, or copy into our fixtures dir for this PR). Asserts `serde_json::from_str::<FleetResolved>(input).unwrap().to_string_canonical() == input_canonicalized`.

### Step 0 — verify_artifact logic

Per RFC-0002 §4 step 0:

1. Deserialize `signed_bytes` as `serde_json::Value`. Error → `VerifyError::Parse`.
2. Re-canonicalize the Value using `nixfleet_canonicalize::canonicalize` — this gives us the byte sequence the signer signed.
3. Verify `signature` against the canonicalized bytes using `pubkey.verify_strict(canonical_bytes, &Signature::from_bytes(signature))`. On `Err` → `VerifyError::BadSignature`.
4. Now safe to re-parse the Value as `FleetResolved`. (We parsed once at step 1 just to route to canonicalize; now we type-safely deserialize into the schema.) Schema error → `VerifyError::Parse`.
5. Check `schemaVersion`: if not 1, `VerifyError::SchemaVersionUnsupported`.
6. Check `meta.signedAt`: if `None`, `VerifyError::Stale { signed_at: None, ... }` (unsigned artifacts rejected for production use). If `Some(t)`:
   - If `now - t > freshness_window` → `VerifyError::Stale { ... }`.
   - Else → return `Ok(fleet)`.

### reconcile logic

Promoted from the spike (`spike/reconciler/src/main.rs` lines 80–162), split across modules per Approach 2. No behavioral change from the spike except:

- `reconcile.rs::reconcile` is the top-level fn matching the spike's.
- `count_in_flight` moves to `rollout_state.rs`.
- `predecessors_done` moves to `edges.rs`.
- Budget logic (lines 102–107 + 125–133) moves to `budgets.rs`.
- Per-host state dispatching (lines 111–149) moves to `host_state.rs`.

Types in the spike (`Fleet`, `Host`, `Channel`, ...) are replaced by the `nixfleet_proto::FleetResolved` import. `Observed`, `HostState`, `Rollout` stay as internal types in `observed.rs`.

## API / Interface

```rust
// crates/nixfleet-reconciler/src/action.rs
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    OpenRollout      { channel: String, target_ref: String },
    DispatchHost     { rollout: String, host: String, target_ref: String },
    PromoteWave      { rollout: String, new_wave: usize },
    ConvergeRollout  { rollout: String },
    HaltRollout      { rollout: String, reason: String },
    Skip             { host: String, reason: String },
}

// crates/nixfleet-reconciler/src/verify.rs
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("fleet.resolved parse failed: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("signature does not verify against the pinned CI release key")]
    BadSignature,
    #[error("stale artifact: signedAt={signed_at:?}, now={now}, window={window:?}")]
    Stale {
        signed_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        window: Duration,
    },
    #[error("unsupported schemaVersion: {0}")]
    SchemaVersionUnsupported(u32),
    #[error("JCS re-canonicalization failed: {0}")]
    Canonicalize(#[source] anyhow::Error),
}
```

## Data flow

```
      CP tick
         │
         ▼
  read (bytes, sig) from CP storage          ← CP concern, not our crate
         │
         ▼
  verify_artifact(bytes, sig, pubkey, now, window)
         │
         ├── Err(VerifyError) ──▶ CP logs alert, aborts tick
         │
         └── Ok(FleetResolved) ──▶ CP loads Observed from its DB
                                        │
                                        ▼
                            reconcile(&fleet, &observed, now)
                                        │
                                        └──▶ Vec<Action> ──▶ CP persists, emits events
```

`nixfleet-reconciler` is stateless. All state lives in `Observed` (CP's DB projection). A cold-restarted CP with the same `(fleet, observed, now)` tuple produces the same action plan — that's what every fixture test asserts.

## Components (files)

See Approach section above for the file tree. One-line responsibilities:

- `nixfleet-proto/src/lib.rs` — module declarations, serde posture documentation.
- `nixfleet-proto/src/fleet_resolved.rs` — `FleetResolved` + 11 nested types.
- `nixfleet-reconciler/src/lib.rs` — public re-exports, nothing else.
- `nixfleet-reconciler/src/verify.rs` — `verify_artifact` + `VerifyError`.
- `nixfleet-reconciler/src/reconcile.rs` — top-level `reconcile` fn.
- `nixfleet-reconciler/src/rollout_state.rs` — RFC-0002 §3.1 transitions.
- `nixfleet-reconciler/src/host_state.rs` — RFC-0002 §3.2 transitions.
- `nixfleet-reconciler/src/budgets.rs` — disruption budget evaluation.
- `nixfleet-reconciler/src/edges.rs` — edge predecessor check.
- `nixfleet-reconciler/src/observed.rs` — `Observed`, `HostState`, `Rollout` internal types.
- `nixfleet-reconciler/src/action.rs` — `Action` enum.

## Edge Cases

- **Unsigned artifact (`meta.signedAt: null`).** Rejected by `verify_artifact` with `Stale { signed_at: None, ... }`. Test-only fixtures (including Stream B's `empty-selector-warns.resolved.json`) use this path but never flow through verify.
- **Artifact at exactly `freshness_window`.** Interpretation: `now - signedAt > freshness_window` (strictly greater) → stale. Equal → still valid. Matches RFC-0002 §4 step 0 literal wording.
- **Clock skew.** Per RFC-0003 §8, all deadlines carry ≥60s slack. `verify_artifact` does not add slack itself — that's the CP's concern to pass `now` with accounting. We just check `now - signedAt > window`.
- **Missing channel in fleet.waves** (Observed references a channel the new FleetResolved doesn't have). Keep spike behavior: `continue` silently. Rationale: this is a transient drift case — the next reconcile tick may see the channel return; aggressively halting on every missing-channel read would amplify noise. A future PR may surface this as an event if operators need visibility. Flagged in Open Questions.
- **Offline host mid-wave.** Per RFC-0002 §5.2: stays Queued indefinitely, does not block wave progression. Fixture `tests/fixtures/host/offline_skipped/` asserts this.
- **Disruption budget exhausted.** `Action::Skip { host, reason }` emitted; host stays in Queued. Fixture asserts `reason` contains budget numbers.
- **Edge predecessor incomplete.** `Action::Skip` with reason `"edge predecessor <name> incomplete"`. Cross-channel edges ignored (RFC-0002 §4.1) — untested in this PR because it requires multi-channel fixture setup; noted as a gap, flagged open.
- **Malformed observed state.** Per design, `reconcile()` must not panic. Invariant violations (e.g., `Rollout.current_wave` out of range) trigger `Action::ConvergeRollout` (treat as "nothing more to do"). `debug_assert!` for internal invariants; no `panic!`.
- **Reconciler called with `Observed` referencing hosts not in `FleetResolved.hosts`.** Spike returns empty action list; promoted adds `Action::Skip { host, reason: "host not in fleet.resolved" }` for observability.

## Test Strategy

### Unit tests (inline `#[cfg(test)] mod tests`)

Small pure helpers get unit tests colocated:
- `budgets::in_flight_count` — 4 cases (empty, 1, several, over-budget).
- `edges::predecessors_done` — 3 cases (no edges, predecessor done, predecessor not done).
- Other helper fns as they arise.

### Integration tests (tests/*.rs, fixture-based)

Fixture triple per transition: `tests/fixtures/<category>/<name>/{fleet,observed,expected}.json`.

**Fixture harness** (single implementation, used by all integration tests):

```rust
// tests/common/mod.rs (shared between test files via #[path])
pub fn run_fixture(name: &str) -> (Vec<Action>, Vec<Action>) {
    let dir = format!("tests/fixtures/{name}");
    let fleet: FleetResolved = load_json(&format!("{dir}/fleet.json"));
    let observed: Observed = load_json(&format!("{dir}/observed.json"));
    let expected: Vec<Action> = load_json(&format!("{dir}/expected.json"));
    let now = DateTime::parse_from_rfc3339("2026-04-24T10:00:00Z").unwrap().with_timezone(&Utc);
    let actual = reconcile(&fleet, &observed, now);
    (actual, expected)
}
```

Each test:

```rust
#[test]
fn pending_to_planning() {
    let (actual, expected) = run_fixture("rollout/pending_to_planning");
    assert_eq!(actual, expected);
}
```

### Fixture inventory (~15 for coverage B)

**Rollout-level (RFC-0002 §3.1, 7 fixtures):**
- `pending_to_planning` — new channel ref, compliance-static-gate passes, opens rollout.
- `planning_to_executing` — waves computed, first wave WaveActive.
- `wave_active_to_soaking` — all hosts in wave Healthy + Soaked.
- `wave_soaking_to_promoted` — soak elapsed, runtime probes pass, advance wave.
- `all_waves_converged` — last wave soaked → Converged.
- `onfailure_rollback_and_halt` — `healthGate.maxFailures` exceeded with policy `rollback-and-halt` → Reverting → Reverted.
- `onfailure_halt` — `healthGate.maxFailures` exceeded with policy `halt` → rollout frozen, in-flight hosts complete naturally, no forced rollback (RFC-0002 §5.1).

**Host-level (§3.2, 5 fixtures):**
- `queued_to_dispatched` — host online, no blocking predecessor, budget ok.
- `confirmwindow_timeout_reverted` — deadline passed with no phone-home → Reverted.
- `healthy_to_soaked` — Healthy + `now - activated_at > soakMinutes` → Soaked.
- `host_failed_triggers_halt` — one host Failed, policy `rollback-and-halt` → rollout halted.
- `offline_host_skipped` — host offline at wave start → `Skip`, wave progresses without it.

**Budgets and edges (4 fixtures):**
- `budget_exhausted_skip` — `maxInFlight` met, next host gets `Skip`.
- `budget_across_rollouts` — two concurrent rollouts respect same budget.
- `edge_predecessor_blocks` — predecessor incomplete → `Skip`.
- `edge_predecessor_done_dispatch` — predecessor Converged → host Dispatched.

### Verify tests (tests/verify.rs, no fixtures)

Keypair generated in-test. Fleet.resolved canonicalized via `nixfleet_canonicalize`, signed, verified:
- `verify_ok` — good signature + fresh timestamp → returns `Ok(FleetResolved)`.
- `verify_bad_signature` — flip a byte in the signature → `BadSignature`.
- `verify_stale` — `signedAt` older than `freshness_window` → `Stale`.
- `verify_unsigned` — `meta.signedAt: None` → `Stale { signed_at: None, ... }`.
- `verify_unsupported_schema` — `schemaVersion: 2` → `SchemaVersionUnsupported`.
- `verify_malformed` — not JSON → `Parse`.
- `verify_tampered_payload` — flip a byte in the bytes after signing → `BadSignature`.

### Proto roundtrip tests (nixfleet-proto/tests/roundtrip.rs)

Hybrid: hand-crafted fixtures for systematic coverage, plus one copied Stream B fixture as a real-Nix-output sanity check. Rationale: Stream B's fixtures were authored for their own tests (empty-selector-warns, tight-budget-warns) and don't systematically exercise every non-spike field. Hand-crafting lets us target each schema addition explicitly; one Stream B copy pins that we actually match what their Nix produces today.

- `hand_crafted_every_nullable_field_roundtrips` — fixture at `tests/fixtures/every-nullable.json` exercising: `hosts.<n>.{closureHash, pubkey}: null`, `meta.{signedAt, ciCommit}: null`, `selector.channel: null`, `channels.<n>.compliance.frameworks: []`, `edges: []`, `disruptionBudgets: []`, `waves.<chan>[0].hosts: []`, `healthGate: {}`. Parse → `FleetResolved` → re-serialize through `nixfleet_canonicalize::canonicalize` → byte-equal to a pre-canonicalized `tests/fixtures/every-nullable.canonical` (analogous to the JCS golden-file pattern from #16).
- `hand_crafted_signed_artifact_roundtrips` — same shape but with `meta.signedAt: "2026-04-24T10:00:00Z"`, `meta.ciCommit: "abc123"`. Exercises the "all metadata present" path.
- `stream_b_empty_selector_roundtrips` — one copy of Stream B's `empty-selector-warns.resolved.json` (copied to `tests/fixtures/stream-b/empty-selector-warns.resolved.json`) as real-Nix-output validation. Test header documents the source path on Stream B's branch and the commit SHA the copy was made from; if Stream B changes schema, the roundtrip fails loudly and we re-copy + commit.
- `unknown_fields_are_ignored` — inject `futureField: "v2-preview"` into the hand-crafted fixture, assert parse succeeds (CONTRACTS §I #1 forward-compat).

## Acceptance criteria

- `cargo test -p nixfleet-proto` green.
- `cargo test -p nixfleet-reconciler` green (~28 tests: ~5 unit, 16 integration, 7 verify).
- `cargo nextest run --workspace` green (pre-push gate) — no regression in any v0.1 crate.
- `nix fmt -- --no-cache --fail-on-change` clean.
- Every RFC-0002 §3 transition (§3.1 and §3.2) has at least one fixture asserting its expected action list.
- `verify_artifact` rejects every class of bad input (see verify tests).
- No `crates/{agent,cli,control-plane,shared,nixfleet-canonicalize}` changes in this PR.
- No `docs/CONTRACTS.md` changes in this PR.
- No `lib/`, `modules/`, `spike/` changes.
- PR stacks cleanly on `feat/12-canonicalize-jcs-pin`; ready to rebase onto `main` when #16 merges.

## Open Questions (genuinely deferred, not silently resolved)

1. **Cross-channel edges.** RFC-0002 §4.1 says edges are rollout-local; cross-channel edges are a v2 goal. Not tested here. Noted.
2. **Re-entry when a host returns from long offline.** RFC-0002 §8 Q1: "current only" is the proposed direction but not yet normative. Fixture tests the current-ref behavior (host returns → gets current target). A future PR may add fixture for skipped-intermediate-rollouts if the design lands.
3. **Scheduler fairness across many concurrent channels.** RFC-0002 §8 Q4: FIFO on rollout start time. Not tested here because no fixture spans enough concurrent channels for this to matter. Acceptable gap.
4. **Stream B fixture drift during review.** Stream B's PR is unmerged. If their fixtures change shape before our PR merges, the roundtrip test must be updated. Mitigation: pin the fixtures we consume to a specific commit SHA on their branch in the test's doc comment; rebase when they merge.
5. **Missing-channel drift observability.** Current decision: silent `continue` matching spike. Future PR may emit an event (not an `Action`) so operators see "rollout X stuck because channel Y missing". Defer until there's an operator complaint or observability PR.

## Files

| File | Action | LOC estimate |
|---|---|---|
| `crates/nixfleet-proto/Cargo.toml` | create | 22 |
| `crates/nixfleet-proto/src/lib.rs` | create | 40 (module doc + re-export) |
| `crates/nixfleet-proto/src/fleet_resolved.rs` | create | ~200 |
| `crates/nixfleet-proto/tests/roundtrip.rs` | create | ~100 |
| `crates/nixfleet-proto/tests/fixtures/every-nullable.json` | create (hand-crafted) | ~60 |
| `crates/nixfleet-proto/tests/fixtures/every-nullable.canonical` | create (hand-verified canonical bytes) | 1 line |
| `crates/nixfleet-proto/tests/fixtures/signed-artifact.json` | create (hand-crafted, meta.signedAt set) | ~60 |
| `crates/nixfleet-proto/tests/fixtures/stream-b/empty-selector-warns.resolved.json` | copy from Stream B `feat/mkfleet-promotion` | ~75 |
| `crates/nixfleet-reconciler/Cargo.toml` | create | ~30 |
| `crates/nixfleet-reconciler/src/lib.rs` | create | 25 (re-exports) |
| `crates/nixfleet-reconciler/src/action.rs` | create | ~30 |
| `crates/nixfleet-reconciler/src/observed.rs` | create | ~50 |
| `crates/nixfleet-reconciler/src/verify.rs` | create | ~120 |
| `crates/nixfleet-reconciler/src/reconcile.rs` | create | ~80 |
| `crates/nixfleet-reconciler/src/rollout_state.rs` | create | ~100 |
| `crates/nixfleet-reconciler/src/host_state.rs` | create | ~120 |
| `crates/nixfleet-reconciler/src/budgets.rs` | create | ~50 |
| `crates/nixfleet-reconciler/src/edges.rs` | create | ~40 |
| `crates/nixfleet-reconciler/tests/common/mod.rs` | create | ~30 |
| `crates/nixfleet-reconciler/tests/rollout_transitions.rs` | create | ~70 (7 test fns) |
| `crates/nixfleet-reconciler/tests/host_transitions.rs` | create | ~50 (5 test fns) |
| `crates/nixfleet-reconciler/tests/budgets_and_edges.rs` | create | ~40 (4 test fns) |
| `crates/nixfleet-reconciler/tests/verify.rs` | create | ~150 (7 test fns) |
| `crates/nixfleet-reconciler/tests/fixtures/rollout/*/{fleet,observed,expected}.json` | create | 7 × ~100 = 700 |
| `crates/nixfleet-reconciler/tests/fixtures/host/*/{fleet,observed,expected}.json` | create | 5 × ~100 = 500 |
| `crates/nixfleet-reconciler/tests/fixtures/budgets_edges/*/{fleet,observed,expected}.json` | create | 4 × ~100 = 400 |
| `Cargo.lock` | auto | — |

**Approximate totals:** ~2000 lines total, of which ~1500 are test fixtures (JSON). Rust code: ~900 LOC spread across 20 source files. Each source file ≤ 250 LOC.

## Plan-phase hand-off

This spec is the input to `superpowers:writing-plans`. The plan will break the above into tasks of ≤200 LOC each, with explicit TDD ordering per module (test-first for every public fn, integration fixtures written alongside the module that consumes them).
