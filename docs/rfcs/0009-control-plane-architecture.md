# RFC-0009: v0.2 control-plane architecture

**Status.** Draft (slated for v0.2 cycle alongside RFC-0008).
**Depends on.** RFC-0001 (fleet topology), RFC-0002 (reconciler), RFC-0003 (agent/CP protocol), RFC-0008 (event-driven host-rollout state).
**Supersedes.** RFC-0002 §5 (today's reconciler tick loop) and the scattered side-effect organisation of `crates/nixfleet-agent/src/*` and `crates/nixfleet-control-plane/src/server/reconcile.rs`.
**Scope.** Implementation skeleton — how the per-host state machine (RFC-0008 §3) and the channel-level planner (this RFC §4) are organised in code so they remain pure, testable, and bug-resistant. Defines the actor-pattern runtime that wraps each pure core, the effect vocabulary they emit, and the explicit responsibility split between CP and agent. Does not change wire protocol (RFC-0008), trust model (RFC-0005), or topology (RFC-0001).
**Implementation.** Refactor of `crates/nixfleet-control-plane/src/server/reconcile.rs` into `crates/nixfleet-reconciler/src/planner.rs` (pure) + `crates/nixfleet-control-plane/src/runtime.rs` (impure runner). Refactor of `crates/nixfleet-agent/src/` into a single `runtime.rs` + the existing module set demoted to background workers + effect handlers. New `crates/nixfleet-state-machine/` crate for the per-host pure `step()` function (shared between agent and CP-mirror view).

## 1. Problem statement

The pre-v0.2 codebase organises side effects as scattered mutable state:

- **Agent.** `pipeline.rs` calls `switch-to-configuration` directly. `health.rs` writes probe results into `ProbeStateCache` (RwLock). `compliance.rs` posts compliance events through its own `Reporter`. `dispatch/rollback.rs` mutates state in response to inbound `RollbackSignal`. Each module is its own actor; they communicate by writing into shared mutable state with no enforced ordering. Tests need to mock the whole I/O surface to assert any single invariant.
- **CP.** `reconcile.rs` interleaves DB writes, signature verification, action emission, and side-effect dispatch in a single 1000-line tick function. The `health_sweep` block consults `now()` directly. Channel-edges read DB state in one helper, planner reads DB state in another, dispatch endpoint reads DB state in a third — all three deriving subtly different views of the same underlying truth. Bugs hide in the variations (the v0.2.0 `c3ab9d75` `terminal_at` fix is one example).

This produces a recurring class of defect — **two readers of "the same state" disagree because they each construct their view differently from primitive DB rows**. Every per-host bug we shipped in v0.2.0 polish (probe gate stale data, sweep timing drift, channel-edge over-hold) lives in this layer, not in the wire protocol or the trust model.

RFC-0008 makes the wire event-driven. This RFC makes the **code** event-driven, so the same bugs cannot reappear in the next layer down.

## 2. Design principles

1. **Functional core, imperative shell.** Every state-affecting decision lives in a pure function: `(state, input, now) → (new_state, [effects])`. Effects are descriptive data, not executions. A separate runner interprets effects against real I/O. The pure core is `#![forbid(unsafe_code)]`-safe by construction and `proptest`-friendly by structure.
2. **One state, one mutator.** Both agent and CP have **exactly one** task that mutates the state machine. Background workers (probes, manifest poller, long-poll listener) emit *events* into a single MPSC channel; the mutator consumes events serially. No locks on rollout state. No interleaved transitions.
3. **Effects-as-data.** The state machine returns `Vec<Effect>`. Effects are an enum of concrete operations (write DB row, send HTTP request, log a metric, fire a systemd unit). The runner has one match-arm per variant. Adding a new effect = adding an enum variant + an arm; the compiler tells you what you forgot.
4. **Same code, both sides.** The per-host state-machine reducer (RFC-0008 §3) is one crate (`nixfleet-state-machine`) used by both the agent runtime and the CP-mirror view. Identical transition semantics on both sides by construction — the compiler enforces what tests would otherwise have to.
5. **Explicit responsibility ledger.** CP and agent each have a written list of "what I'm responsible for" (§5 below). Anything not on the list is the other side's job. Cross-references prevent drift.
6. **Replay-friendly.** Every decision has its inputs visible: state + event + now. An event log replayed through the reducer reproduces the exact transition sequence. Operators can answer "what would have happened if event X arrived 5 s later" without a live system.

## 3. Per-host state machine (`nixfleet-state-machine` crate)

```rust
// crates/nixfleet-state-machine/src/lib.rs

/// Pure reducer. Same signature on agent and CP-mirror sides.
/// `now` is a parameter so tests can advance time deterministically.
pub fn step(
    state: HostRolloutState,
    event: Event,
    now: DateTime<Utc>,
    policy: &RolloutPolicy,        // from signed manifest
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError>;

pub enum Event {
    // Inputs the agent runtime synthesises from local activity:
    LocalActivationStarted { closure: ClosureHash, at: DateTime<Utc> },
    LocalActivationCompleted { observed: ClosureHash, exit_code: i32, at: DateTime<Utc> },
    LocalActivationFailed { exit_code: i32, stderr_tail: String, at: DateTime<Utc> },
    LocalProbeObserved { name: String, status: ProbeStatus, at: DateTime<Utc> },
    LocalSustainedFailureCrossed { threshold_secs: u64, at: DateTime<Utc> },
    LocalRollbackCompleted { reverted_to: ClosureHash, at: DateTime<Utc> },

    // Inputs the agent receives from CP via long-poll:
    DispatchReceived { rollout_id: RolloutId, target: ClosureHash, soak_due_at: DateTime<Utc> },

    // Inputs the CP runtime synthesises from inbound agent events (mirrors LocalXXX):
    RemoteDispatchAck { ... },
    RemoteActivationStarted { ... },
    // ... one per RFC-0008 §4.2 event
}

pub enum Effect {
    // Side-effect descriptions. Agent runtime executes Local*; CP runtime executes Remote*.
    LocalFireSwitch { target: ClosureHash },
    LocalFireRollbackTo { closure: ClosureHash },
    LocalResetProbeCache,                                  // RFC-0008 §4.2 ActivationComplete
    LocalEmitEvent { payload: AgentEvent },                // outbound to CP
    RemoteQueueDispatch { host: HostId, rollout: RolloutId },
    RemoteRecordTransition { from: HostRolloutState, to: HostRolloutState, at: DateTime<Utc> },
    RemoteInsertQuarantine { channel: ChannelId, closure: ClosureHash },
    EmitMetric { name: &'static str, labels: Vec<(&'static str, String)> },
    EmitLog { level: Level, fields: HashMap<&'static str, String>, message: &'static str },
}
```

**Properties enforced by the reducer's type signature:**
- Cannot mutate state outside `step`.
- Cannot perform I/O inside `step` (no `async`, no `Result` on Tokio types, no `&mut Database`).
- Cannot read `now` from `chrono::Utc::now()` — must be parameter.
- Cannot read manifest policy from anywhere except the `policy: &RolloutPolicy` argument.

Tests:

```rust
#[test]
fn soak_does_not_fire_before_first_probe() {
    let s0 = HostRolloutState::activating(...);
    let (s1, _) = step(s0, Event::LocalActivationCompleted { ... }, t0, &CANARY).unwrap();
    let (s2, _) = step(s1, Event::AdvanceTime(t0 + soakMinutes), t0 + soakMinutes, &CANARY).unwrap();
    assert_eq!(s2.state, HostState::Soaking);  // NOT Converged - no probe observed yet
    let (s3, _) = step(s2, Event::LocalProbeObserved { status: Pass, .. }, t1, &CANARY).unwrap();
    assert_eq!(s3.state, HostState::Converged);  // Now Converged
}

proptest! {
    #[test]
    fn no_event_sequence_violates_invariants(events in arb_event_sequence()) {
        let mut state = HostRolloutState::initial();
        for (event, now) in events {
            if let Ok((next, _)) = step(state.clone(), event, now, &arb_policy()) {
                assert!(invariant_current_matches_declared_when_converged(&next));
                assert!(invariant_no_negative_soak_window(&next));
                state = next;
            }
        }
    }
}
```

Every bug from v0.2.0 polish becomes a one-line property. Regression resistance free.

## 4. The planner (`nixfleet-reconciler` crate, refactored)

CP's reconciler split into two layers — pure planner and impure applier — exactly mirroring the per-host pattern.

### 4.1 Pure planner

```rust
// crates/nixfleet-reconciler/src/planner.rs

pub fn plan_next(
    manifests: &SignedManifestSet,        // verified, freshness-validated
    fleet_state: &FleetState,             // derived from event log; see §4.2
    quarantines: &QuarantineSet,
    now: DateTime<Utc>,
) -> Vec<PlanAction>;

pub enum PlanAction {
    OpenRollout { channel: ChannelId, target_ref: ChannelRef, rollout_id: RolloutId },
    QueueDispatch { host: HostId, rollout: RolloutId, target: ClosureHash, soak_due_at: DateTime<Utc> },
    MarkChannelTerminal { channel: ChannelId, rollout: RolloutId },
    InsertQuarantine { channel: ChannelId, closure: ClosureHash, reason: QuarantineReason },
    ClearStaleQuarantine { channel: ChannelId, closure: ClosureHash },
    RecordHaltLifted { channel: ChannelId },
}

pub struct FleetState {
    pub host_states: HashMap<HostId, HostRolloutState>,
    pub active_rollout_per_channel: HashMap<ChannelId, RolloutId>,
    pub rollouts: HashMap<RolloutId, RolloutSummary>,
}
```

`plan_next` is pure: no `now` from `chrono`, no DB reads, no HTTP. The runner builds `FleetState` from the event log + DB cache before each call.

### 4.2 Gates as pure boolean functions

```rust
// crates/nixfleet-reconciler/src/gates/

pub fn channel_edges_block(
    fleet: &FleetState,
    manifests: &SignedManifestSet,
    successor: &ChannelId,
) -> Option<GateBlock>;

pub fn wave_promotion_block(
    fleet: &FleetState,
    host: &HostId,
    rollout: &RolloutId,
) -> Option<GateBlock>;

pub fn disruption_budget_block(
    fleet: &FleetState,
    host: &HostId,
    manifest: &Manifest,
) -> Option<GateBlock>;

pub fn quarantine_block(
    quarantines: &QuarantineSet,
    channel: &ChannelId,
    target: &ClosureHash,
) -> Option<GateBlock>;
```

Each gate consults only its inputs. No more `is_active_for_ordering()` heuristics — `FleetState.host_states.get(predecessor_host_in_predecessor_channel).is_converged()` is a direct read.

### 4.3 Impure applier

```rust
// crates/nixfleet-control-plane/src/runtime.rs

pub async fn apply_plan(actions: Vec<PlanAction>, deps: &CpDeps) -> Result<()>;
```

One match per `PlanAction` variant. Writes to DB, queues HTTP responses for the next agent long-poll, fires metrics. The compiler ensures every variant is handled.

## 5. CP responsibility ledger

Every responsibility CP has in v0.2. Anything not on this list is **not** CP's job.

| # | Responsibility | Mechanism | Source of truth |
|---|---|---|---|
| 1 | Verify and cache signed manifests | `fleet-poll` task fetches from forge, runs `nixfleet-verify-artifact`, persists to DB | forge's CI release-signing key (RFC-0002 §3) |
| 2 | Open rollouts on new channel refs | Planner emits `OpenRollout` action | signed `fleet.resolved.json` |
| 3 | Queue per-host `Dispatch` (timing/wave signal only) | Planner emits `QueueDispatch`; runner places it on the agent's next long-poll response to `/v1/agent/dispatch` | planner output |
| 4 | Enforce wave promotion + disruption budget + channel-edges | Pure gate functions (§4.2) | `FleetState` from event log |
| 5 | Maintain per-channel quarantine table | Apply `InsertQuarantine` / `ClearStaleQuarantine` actions in response to `RollbackComplete` events from agents | aggregated agent events |
| 6 | Append-only event log | `runtime::ingest_event` writes signed events to DB on receipt | inbound agent POST to `/v1/agent/events` |
| 7 | Heartbeat liveness monitoring | `heartbeat-watcher` task flags hosts with missed heartbeats | `host_rollout_records.last_heartbeat_at` |
| 8 | Serve operator API | `/v1/operator/*` endpoints read from event log + cache | event log |
| 9 | Serve agent endpoints | `/v1/agent/dispatch` long-poll, `/v1/agent/events` POST, `/v1/agent/heartbeat` POST, `/v1/manifests/<ref>` GET | various |

### What CP explicitly does NOT do

| # | Non-responsibility | Why |
|---|---|---|
| N1 | Decide per-host rollback | Agent reads `onHealthFailure` policy from the signed manifest directly (RFC-0008 §2.1, §4.1) |
| N2 | Run the sustained-failure sweep | Agent times its own `HEALTH_FAILURE_THRESHOLD_SECS` from its own probe-cache (RFC-0008 §6) |
| N3 | Infer host state from checkin diffs | Agent emits explicit events (RFC-0008 §4.2) |
| N4 | Reset probe state on activation | Agent does it locally (`LocalResetProbeCache` effect) |
| N5 | Hold trust private keys | RFC-0005 §1.5 — exception: the CA-issuance key, TPM-bound in production per RFC-0005 §1.5.1 |
| N6 | Sign anything | RFC-0002 §3 — CP is a verified stateless distributor for manifests and events. **Amendment (2026-05-17):** CP does sign agent mTLS certs at `/v1/enroll` and `/v1/agent/renew-cert`; the signing material is TPM-resident in production (RFC-0005 §1.5.1). The "stateless distributor" claim continues to hold for the manifest + event pipeline. |
| N7 | ~~Forge secrets, mint bootstrap tokens, generate certs~~ Forge secrets, mint bootstrap tokens | Bootstrap-token authority remains outside CP (org-root threshold per RFC-0005). **Agent cert generation now happens inside CP** at enroll/renew time per RFC-0005 §1.5.1; the signing key is TPM-bound in production. |
| N8 | Decide what closure a host should run | Signed manifest specifies it; agent verifies it; CP just routes |

The list is short by design. When in doubt: "could an agent figure this out from the signed manifest plus its own local state?" If yes, agent does it.

> **Amendment note (2026-05-17).** N5/N6/N7 above were rewritten to reflect the CA-issuance signing path that landed in `feat(cp,trust): cert issuance` (commit `4808d4dc`). The architectural intent — production-grade deployments hold no in-memory signing material — is preserved via the TPM-backed `CaSigner` backend; the file-backed backend is a dev convenience. RFC-0005 §1.5.1 is the canonical statement; an additive `--strict` enforcement closes the operator-facing gap. Adjacent claims in RFC-0008 §2.1 are amended in the same pass.

## 6. Agent responsibility ledger

| # | Responsibility | Mechanism | Source of truth |
|---|---|---|---|
| 1 | Verify signed manifest, hold canonical declared state | `nixfleet-verify-artifact` on every fetch (RFC-0003 §4.6) | manifest signature against `nixfleet.trust.ciReleaseKey` |
| 2 | Run health probes against `/version`, etc. | `probe-worker` background tasks | per-probe HTTP / TCP / exec result |
| 3 | Detect sustained probe failure | Local timer in the state machine reducer | `LocalProbeObserved` events |
| 4 | Decide rollback (from manifest policy) | Reducer reads `policy: &RolloutPolicy` arg, transitions to `RollbackComplete` automatically when `onHealthFailure = "rollback-and-halt"` | signed manifest |
| 5 | Execute switch-to-configuration (forward + rollback) | `LocalFireSwitch` / `LocalFireRollbackTo` effects → `systemd-run` | reducer-emitted effect |
| 6 | Emit signed events to CP | `LocalEmitEvent` effect → HTTP POST `/v1/agent/events` | reducer output |
| 7 | Persist outbound event queue across restarts | Durable on-disk queue (§7.2 of RFC-0008) | local fs |
| 8 | Heartbeat | `heartbeat-worker` task posts `/v1/agent/heartbeat` every 60 s | local clock + reducer's `last_event_seq` |
| 9 | Reconcile on boot | Read `/run/current-system`, emit synthetic `ActivationComplete` if needed (RFC-0008 §9.5) | local filesystem |

## 7. Runtime topology

### 7.1 Agent runtime

```
                ┌────────────────────────────────────────────────┐
                │                Agent process                   │
                │                                                │
                │   [probe-worker]──┐                            │
                │   [activation-w]──┼─►mpsc::Sender<Event>───┐   │
                │   [longpoll-w]────┤                        │   │
                │   [heartbeat-w]───┘                        ▼   │
                │                              ┌──────────────┐  │
                │                              │  Reducer     │  │
                │                              │  loop        │  │
                │   ┌─────────────────────────►│  step() ────►│  │
                │   │                          └──────┬───────┘  │
                │   │                                 │          │
                │   │ effects: LocalFireSwitch,       │ Vec<Eff> │
                │   │ LocalEmitEvent, ...             ▼          │
                │   │                          ┌──────────────┐  │
                │   │                          │  Runner      │  │
                │   └──────────────────────────│  apply()     │  │
                │                              └──────────────┘  │
                │                                                │
                └────────────────────────────────────────────────┘
```

- One MPSC channel feeds the reducer loop.
- The reducer is single-task, single-threaded. No locks needed on state.
- Workers can run on any tokio threads; only the reducer is serial.

### 7.2 CP runtime

Same shape, different effect handlers:

```
                ┌────────────────────────────────────────────────┐
                │                  CP process                    │
                │                                                │
                │   [manifest-poll]──┐                           │
                │   [event-ingest]───┼─►mpsc::Sender<CpEvent>──┐ │
                │   [heartbeat-rx]───┤                         │ │
                │   [op-api]─────────┘                         ▼ │
                │                              ┌──────────────┐  │
                │                              │  Reducer     │  │
                │                              │  loop        │  │
                │   ┌─────────────────────────►│  plan_next() │  │
                │   │                          └──────┬───────┘  │
                │   │                                 │          │
                │   │ actions: QueueDispatch,         │ Vec<Act> │
                │   │ InsertQuarantine, ...           ▼          │
                │   │                          ┌──────────────┐  │
                │   │                          │  Applier     │  │
                │   └──────────────────────────│  apply()     │  │
                │                              └──────────────┘  │
                │                                                │
                └────────────────────────────────────────────────┘
```

The CP reducer additionally maintains a **mirror** of per-host state, derived by running each inbound `AgentEvent` through the same `nixfleet-state-machine::step()` function the agent uses. Two consequences:

- CP and agent **cannot disagree** about a host's state for any given event sequence — same code, same input.
- Tests that prove the per-host machine correct (proptest) automatically prove the CP mirror correct.

## 8. Crate layout (target end-state)

```
crates/
  nixfleet-state-machine/      [NEW] Pure per-host reducer (RFC-0008 §3)
    src/
      lib.rs                   step(), state types, effect enum
      transitions/             one module per state transition group
      tests/                   proptest invariants

  nixfleet-reconciler/         [REFACTOR] Pure planner + gates
    src/
      planner.rs               plan_next()
      gates/                   one module per gate (channel-edges, waves, budgets, quarantine)
      tests/

  nixfleet-control-plane/      [REFACTOR] Impure runner + DB + HTTP
    src/
      runtime/
        mod.rs                 single-task reducer loop
        applier.rs             interprets PlanAction + Effect
        workers/               manifest-poll, event-ingest, heartbeat-rx, op-api
      server/                  /v1/* HTTP routes
      db/                      cache (HostRolloutRecord, event log, quarantines)

  nixfleet-agent/              [REFACTOR] Impure runner + workers + activation
    src/
      runtime/
        mod.rs                 single-task reducer loop
        applier.rs             interprets Effect (LocalFireSwitch, LocalEmitEvent, ...)
        workers/               probe, activation, longpoll, heartbeat
      activation/              switch-to-configuration glue (called from applier)
      compliance/              compliance probe runner (emits events)
      enrollment.rs            unchanged

  nixfleet-proto/              [UNCHANGED + new event types] wire schemas (RFC-0008 §4)

  nixfleet-verify-artifact/    [UNCHANGED] manifest signature verification
```

Boundaries:

- `nixfleet-state-machine` depends only on `nixfleet-proto`. No tokio, no reqwest, no rusqlite.
- `nixfleet-reconciler` depends only on `nixfleet-state-machine` + `nixfleet-proto`. Same purity restrictions.
- `nixfleet-control-plane` and `nixfleet-agent` are the only crates with I/O. Both consume the pure crates above.

Compile-time enforcement: nothing in `nixfleet-state-machine`'s `Cargo.toml` declares tokio/reqwest/rusqlite as a dependency. The boundary is mechanical.

## 9. Effect vocabulary

Definitive list — both agent and CP runtimes match on this enum:

```rust
pub enum Effect {
    // Agent-only effects (CP runner returns Error if it sees these):
    LocalFireSwitch { target: ClosureHash },
    LocalFireRollbackTo { closure: ClosureHash },
    LocalResetProbeCache,
    LocalEmitEvent { payload: AgentEvent, durable: bool },  // durable=true persists to disk first

    // CP-only effects:
    RemoteQueueDispatch { host: HostId, payload: Dispatch },
    RemoteInsertQuarantine { channel: ChannelId, closure: ClosureHash },
    RemoteClearStaleQuarantine { channel: ChannelId, closure: ClosureHash },
    RemoteOpenRolloutRecord { rollout: RolloutId, channel: ChannelId, target_ref: ChannelRef },
    RemoteAppendEventLog { event: AgentEvent, host: HostId },

    // Shared effects (both runners handle):
    RecordTransition { host: HostId, rollout: RolloutId, from: HostState, to: HostState, at: DateTime<Utc> },
    EmitMetric { name: &'static str, labels: Vec<(&'static str, String)>, value: f64 },
    EmitLog { level: Level, target: &'static str, message: &'static str, fields: HashMap<&'static str, String> },
}
```

Symmetric design: 4 local-only, 5 remote-only, 3 shared. The reducer knows from context which set it can emit; the runner has compile-time assurance every variant is handled.

## 10. Open questions

1. **State machine crate boundary fineness.** Should `nixfleet-state-machine` ship one reducer or multiple (one per "role": host-rollout, channel-rollout, fleet-wide)? Leaning one — separation by responsibility, not by reducer type. Re-visit if compilation time becomes an issue.
2. **Effect type stability across major versions.** `Effect` is internal to the runtime; it doesn't appear on the wire. But it appears in tests and in the trace logs. Should it be `#[non_exhaustive]` to allow additions without semver bumps? Lean yes.
3. **Mirror state on CP — full or projected?** Today's outline says CP runs the same `step()` to maintain a full per-host mirror. Alternative: CP keeps only a projected `FleetView` (Idle / InFlight / Converged / Failed / Reverted — 5 states instead of 6) since that's all the planner needs. Trade-off: full mirror = easier to reason about (same state as agent), projected = smaller memory footprint at scale. Decide before implementation.
4. **Event log durability vs throughput.** Append-only to SQLite per inbound event: fine at homelab scale, may bottleneck at enterprise. Batch + fsync interval? Out of scope here but flag for performance review.
5. **Worker shutdown semantics.** All workers reach into the MPSC. On graceful shutdown, who closes the channel first? Convention: the reducer loop owns the channel's `Sender` (workers hold clones); reducer-loop exit drops its `Sender`, workers see channel closed and exit. Documented but not enforced by types.
6. **Reducer panic recovery.** If `step()` panics on bad input, the agent loses its mutator. Restart via systemd? Catch panic and treat as a TransitionError? Lean catch + error event to CP + continue; we don't want one bad input to kill the whole agent.
7. **Compliance events.** Today's compliance reporting is separate from the rollout state machine. Under this architecture, compliance probes are just another worker emitting `LocalProbeObserved`-shaped events. Decide whether to unify the event vocabulary in this RFC or keep compliance in its own lane (with its own reducer) — both work.

## 11. Validation plan

- **Per-host reducer property tests.** Every legal event sequence preserves the invariants from RFC-0008 §3. `proptest` runs ~10000 sequences per CI run; failure shrinks to minimal reproducer.
- **CP planner property tests.** Same approach against `plan_next`: every fleet state + manifest combination produces a valid plan (no Dispatch to a quarantined SHA, no channel-edges violation, no exceeded budget).
- **Mirror parity tests.** Run the same event sequence through agent's and CP's `step()`; assert final states are byte-identical. Catches drift if any code path ever diverges.
- **Effect-handler exhaustiveness.** Compiler-enforced. New `Effect` variant without a runner arm = build failure. Verified by adding a deliberately-unhandled variant in a test fixture and asserting `cargo build` fails.
- **No-I/O-in-pure-crates check.** CI step: `cargo tree -p nixfleet-state-machine` must not list `tokio`, `reqwest`, `rusqlite`, `chrono` (note: `chrono` allowed for types, but not `chrono::Utc::now`). Same for `nixfleet-reconciler`.
- **Re-run the v0.2 demo cycle.** Six-phase validation (`fleet-up` → `fleet-promote` → `fleet-rollback` → `fleet-recover` → `fleet-down`) against the new runtime. Same assertions as RFC-0008 §10 plus: no shared-mutable-state warnings under `loom` testing of the agent runtime.

## 12. Migration

None. v0.2 is a full rewrite (per RFC-0008 §7). The pre-v0.2 reconciler tick loop, the `host_dispatch_state` table, the `pipeline.rs` / `health.rs` / `compliance.rs` cross-talk — all deleted on the same commit that lands `nixfleet-state-machine`. No compat shim, no parallel code paths.

The new code is smaller. Today's `reconcile.rs` is ~1100 lines doing nine things; this RFC's runtime split is roughly:

| File | LoC estimate |
|---|---|
| `nixfleet-state-machine/src/lib.rs` | ~400 (reducer + types) |
| `nixfleet-reconciler/src/planner.rs` | ~200 |
| `nixfleet-reconciler/src/gates/*.rs` | ~300 (split, 5 gates) |
| `nixfleet-control-plane/src/runtime/applier.rs` | ~250 (one arm per PlanAction + Effect) |
| `nixfleet-control-plane/src/runtime/workers/*` | ~400 (four workers) |
| `nixfleet-agent/src/runtime/applier.rs` | ~150 |
| `nixfleet-agent/src/runtime/workers/*` | ~300 |

Net: roughly the same total LoC, but partitioned along lines that make the bugs we hit structurally impossible to recur.
