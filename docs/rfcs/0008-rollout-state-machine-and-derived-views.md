# RFC-0008: Rollout-level state machine and uniform derived-view discipline

**Status.** Accepted.
**Depends on.** RFC-0005 (event-driven host-rollout state), RFC-0006 (control-plane architecture), RFC-0007 (multi-scope health probes), RFC-0004 (architectural patterns).
**Supersedes.** Ad-hoc rollout lifecycle bookkeeping previously held in independent-table writes of `rollouts` and `quarantined_closures`; both become derived views written by the applier in the same transaction as the canonical `event_log` append.
**Scope.** Two reinforcing changes: (1) elevate rollout lifecycle to a pure state machine in `nixfleet-state-machine` parallel to RFC-0005's per-host machine; (2) make every applier-written CP DB table a derived view with `event_log_seq` foreign-key back to canonical state.

## 1. Problem statement

Two reinforcing architectural gaps surfaced during the v0.2 fold's architectural-reviewer audit (RFC-0004 §4):

### 1.1 Rollout lifecycle is a state machine, but isn't modeled as one

`crates/nixfleet-control-plane/src/db/rollouts.rs` carries rollout lifecycle as scattered boolean methods and SQL UPDATEs:

```rust
is_superseded(&self) -> bool
is_terminal(&self) -> bool
is_finished(&self) -> bool
record_active_rollout(&self, rollout_id, channel) -> Result<()>
supersede_status(&self, rollout_id) -> Result<Option<SupersedeStatus>>
mark_terminal(&self, rollout_id, now) -> Result<usize>
set_current_wave(&self, rollout_id, wave) -> Result<usize>
superseded_rollout_ids() -> Result<Vec<String>>
finished_rollout_ids() -> Result<Vec<String>>
prune_finished_rollouts(&self, retention_hours) -> Result<(usize, usize)>
```

States are implicit (intersections of booleans). Transitions live at applier call sites — no single function answers "what are the legal rollout transitions and what triggers them?" This is the same disease the per-host state had pre-RFC-0005 (RFC-0004 §1). No proptest invariants, no replay tooling, no audit-trail of rollout-level state changes (the `event_log` carries per-host events only).

### 1.2 Two CP tables remain shadow state, not derived views

After RFC-0007's `probe_failures` introduction, the CP DB tables divide into four classes (RFC-0004 §2.4):

| Class | Tables (post-RFC-0007) |
|---|---|
| Reducer state cache | `host_rollout_records` |
| Canonical event log | `event_log` |
| Outbound queue | `dispatch_queue` |
| **Derived view (`event_log_seq` FK-back)** | `probe_failures` |
| **Applier-written, no FK-back (shadow state)** | `rollouts`, `quarantined_closures` |
| Security-critical lookup (TTL lifecycle) | `token_replay`, `cert_revocations` (justified separate; see §6) |

The two shadow-state tables work the same way `host_reports` did before RFC-0007 deleted it: applier writes them, gates read them, but there is no FK-back to `event_log` proving derivability. If a future bug ever desynchronizes them from event_log, divergence is silent until a query surfaces it — exactly the v0.2.0-era bug class the cycle is replacing.

## 2. Design goals

1. **Rollout lifecycle becomes a pure state machine.** Same `step(state, event, now) → (state, Vec<Effect>)` discipline as RFC-0005 §3 per-host state. Lives in `nixfleet-state-machine` alongside the host state machine. Proptest invariants. Replay-friendly.

2. **Every applier-written CP table becomes a derived view.** `rollouts` and `quarantined_closures` gain `event_log_seq` foreign-key primary references; applier co-writes the canonical event_log row and the derived-view row in a single transaction.

3. **One canonical store, derived views provably re-derivable.** If any derived-view table is lost (DB rebuild, schema migration), a walk over `event_log` reconstructs it. The reducer state cache (`host_rollout_records`) and the outbound queue (`dispatch_queue`) are explicit exceptions — they hold work-in-flight state that isn't pure derivation.

4. **Rollout-level events captured in `event_log`.** Today, only per-host events land there. After this RFC, rollout-level transitions (`RolloutOpened`, `RolloutTerminal`, `RolloutSuperseded`) also land, giving operators and replay tools a complete chronological view at both granularities.

5. **No reducer composition headaches.** The rollout state machine consumes a subset of per-host events as inputs (it sees `HostStateChanged` events emitted by the per-host applier) but operates on its own state. The two reducers run sequentially in the same applier transaction; no cross-mutator hazards.

## 3. Rollout state machine

```
                        ┌──────────────────────────────────────────┐
                        ▼                                          │
   ┌─────────┐    ┌──────────┐    ┌────────────┐    ┌─────────────┴───┐
   │ Opening │───▶│  Active  │───▶│ Converging │───▶│    Terminal     │
   └─────────┘    └──────────┘    └────────────┘    └─────────────────┘
        │              │                                     │
        │              ▼                                     │
        │         ┌─────────────┐                            │
        │         │  Reverted   │                            │
        │         └─────────────┘                            │
        │              │                                     │
        │              ▼                                     │
        │         ┌─────────────┐                            │
        │         │   Failed    │                            │
        │         └─────────────┘                            │
        │                                                    │
        └─── superseded ─────────────┐                       │
                                     ▼                       ▼
                              ┌────────────────────────────────────┐
                              │           Superseded               │
                              └──────────────┬─────────────────────┘
                                             │
                                             ▼
                                       ┌─────────────┐
                                       │   Pruned    │
                                       └─────────────┘
```

**Eight states:**

| State | Meaning | Entered by | Exited by |
|---|---|---|---|
| `Opening` | Channel-refs poll detected new ref; rollout opened; no hosts dispatched yet | `RolloutOpened` event | First `HostJoined` event (→ Active) or `SuccessorOpened` (→ Superseded, rare) |
| `Active` | At least one host is in-flight (`Pending`/`Activating`/`Soaking` per RFC-0005) | First `HostJoined` event | All in-flight hosts reach `Soaked` or `Converged` (→ Converging); or any host enters `Failed`/`Reverted` (→ Reverted/Failed) |
| `Converging` | All dispatched hosts reached `Soaked`; later waves remain to dispatch | All current-wave hosts reach Soaked | Next wave dispatched (→ Active); all hosts in all waves Converged (→ Terminal) |
| `Terminal` | All hosts in all waves are `Converged`; channel-edges may release | All hosts Converged | `SuccessorOpened` (→ Superseded) or retention expiry (→ Pruned) |
| `Reverted` | Any host reached `Reverted` via `rollback-and-halt` policy | First host `Reverted` event | Manual `OperatorClearance` (rare) or `SuccessorOpened` (→ Superseded) |
| `Failed` | Any host stuck in `Failed` state without rollback (e.g., `halt-only` policy) | First host `Failed` event with policy != `rollback-and-halt` | Manual `OperatorClearance` or `SuccessorOpened` |
| `Superseded` | A newer rollout for the same channel opened | `SuccessorOpened` event | Retention expiry (→ Pruned) |
| `Pruned` | Retention timeout elapsed; rollout no longer actionable | `RetentionExpired` event | Row persists (table remains re-derivable from `event_log`); physical row deletion deferred to v0.3 retention-compaction. The in-memory state-machine instance is freed; the DB row stays for audit. |

**Invariants enforced by the reducer:**

- `Terminal ⇒ ∀ host ∈ rollout: state == Converged`.
- `Reverted ⇒ ∃ host ∈ rollout: state == Reverted` AND no host is currently in-flight on the original target.
- A `RolloutOpened` event for `(channel, ref)` where the channel's `active_rollout_id != None` is a structural error → reducer returns `TransitionError::SupersessionExpected` (the planner must emit `SuccessorOpened` first).
- `Superseded` is terminal-for-ordering but not terminal-for-pruning. Channel-edges treat `Superseded` like `Terminal`; retention treats them differently.

## 4. Rollout-level events

All events are CP-internal (emitted by the applier as it processes per-host events). They do NOT cross the agent ↔ CP wire — agents only emit per-host events per RFC-0005 §4.2; CP synthesizes rollout-level events from those inputs.

Stored in `event_log` with `kind = 'rollout_event'` (new value alongside the existing `agent_event | plan_action | effect | gate_decision | verify_outcome | manifest_poll`).

```rust
pub enum RolloutEvent {
    RolloutOpened {
        rollout_id: RolloutId,
        channel: ChannelId,
        target_ref: ChannelRef,
        at: DateTime<Utc>,
    },
    HostJoined {
        rollout_id: RolloutId,
        host_id: HostId,
        wave: u32,
        at: DateTime<Utc>,
    },
    HostStateChanged {
        rollout_id: RolloutId,
        host_id: HostId,
        from: HostRolloutState,
        to: HostRolloutState,
        at: DateTime<Utc>,
    },
    WaveAdvanced {
        rollout_id: RolloutId,
        from_wave: u32,
        to_wave: u32,
        at: DateTime<Utc>,
    },
    RolloutTerminal {
        rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    SuccessorOpened {
        superseded_rollout_id: RolloutId,
        successor_rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    RetentionExpired {
        rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    OperatorClearance {
        rollout_id: RolloutId,
        operator: String,
        reason: String,
        at: DateTime<Utc>,
    },
}
```

These mirror the existing `PlanAction` outputs (RFC-0006 §4.1) but with explicit state-machine semantics. The applier emits a `RolloutEvent` into the rollout reducer for each relevant per-host transition, then writes the resulting effects.

## 5. Rollout-level effects

```rust
pub enum RolloutEffect {
    RecordRolloutTransition {
        rollout_id: RolloutId,
        from: RolloutState,
        to: RolloutState,
        at: DateTime<Utc>,
    },
    UpdateCurrentWave {
        rollout_id: RolloutId,
        wave: u32,
    },
    InsertQuarantineFromRollout {
        channel: ChannelId,
        closure_hash: ClosureHash,
        triggering_event_log_seq: i64,
    },
    SchedulePruning {
        rollout_id: RolloutId,
        delay: Duration,
    },
}
```

The applier interprets these effects against the `rollouts` derived-view table. Each effect produces one `event_log` row (the triggering `RolloutEvent`) AND one or more derived-view writes, in a single SQL transaction.

## 6. Derived-view discipline (Lever B)

### 6.1 The rule

A CP DB table is **derived** if and only if:

1. The applier is its only writer.
2. Every row carries an `event_log_seq INTEGER REFERENCES event_log(seq)` column (or a compound key including one). The FK is the proof obligation for re-derivability.
3. The derived-view row is co-written by the applier in tight temporal coupling with the canonical `event_log` append. **Target shape:** single SQL transaction (atomic). **Current v0.2 shape** (matches `probe_failures` in RFC-0007 §7.2): the event_log writer is a fire-and-forget bounded-mpsc task, so the applier inserts the derived-view row with `event_log_seq = NULL` and tightens to NOT NULL once the writer gains synchronous seq return. The eventual-consistency window between the event_log row landing and the derived-view row landing is bounded (single-applier-task ordering) and operator-observable via the prune-timer's audit metric.
4. Walking `event_log` chronologically can reproduce the table from empty.

The looser current shape (item 3) preserves invariants 1, 2, and 4. What is deferred is only the atomicity guarantee against a crash between the mpsc-send and the derived-view insert. Operators monitor this window via the prune-timer metric; a follow-up tightens it to true single-transaction.

### 6.2 Tables and their classifications post-RFC-0008

| Table | Class | Notes |
|---|---|---|
| `event_log` | **Canonical** | Append-only audit; sole source-of-truth |
| `host_rollout_records` | **Reducer state cache** | Per-host state machine cache; rebuilt from event_log on cold start |
| `dispatch_queue` | **Outbound queue** | Work-in-flight, not derivation |
| `probe_failures` | **Derived view** | Already conforms (RFC-0007 §7.2) |
| `rollouts` | **Derived view (RFC-0008 §6.3)** | Migrated from independent-write to applier-co-write with `event_log_seq` FK |
| `quarantined_closures` | **Derived view (RFC-0008 §6.4)** | Migrated similarly |
| `token_replay` | **Security lookup (exception)** | TTL-pruned; different lifecycle than event_log audit. Justified separate. |
| `cert_revocations` | **Security lookup (exception)** | Same as token_replay. |

The two security-lookup tables are the documented exceptions. Any future applier-written table must conform to the derived-view rule.

### 6.3 `rollouts` migration

The `rollout_id` is content-addressed from `(channel, channel_ref)` via the canonical format `"{channel}@{channel_ref}"`. Constructed only via `RolloutId::new(channel, channel_ref)`; the newtype's private inner field prevents ad-hoc construction (same no-public-constructor pattern as `Verified<T>` per RFC-0006 §3, with a test-only escape hatch under `#[cfg(any(test, feature = "test-helpers"))]`). The format choice is operator-visible (appears in CLI output, the `event_log` payload, and rollout-event tag bodies) and matches the existing `display_name` convention.

**`display_name` vs `RolloutId`.** Both carry the `<channel>@<X>` shape but they are NOT interchangeable. `RolloutId` (`{channel}@{channel_ref}`) is the primary key: full `channel_ref` (typically a 40-char git SHA), wire-validated by the CP route, persisted in `rollouts.rollout_id`, and the only value that resolves to a manifest at `GET /v1/rollouts/<rolloutId>`. `display_name` (`{channel}@{short-ci-commit}`) is a producer-supplied, human-skimmable label carried inside the manifest payload — usable in operator surfaces, never used for lookup or equality. The `display_name` field is retained for compatibility with the v0.1 rendering convention and may go away in a future schema bump.

Rationale: two channels can share a `channel_ref` (the architectural point of multi-channel cascading from a single git push). `rollout_id = channel_ref` alone collides in that topology; `rollout_id = channel` alone violates the content-addressed property of the rest of the cycle. The composite encoding preserves both: unique per `(channel, channel_ref)` AND deterministic across replays. Re-derivability from `event_log` walks (RFC-0004 §2.4) holds because the identity is reproducible from the canonical-format inputs alone.

New schema:

```sql
CREATE TABLE rollouts (
    rollout_id            TEXT PRIMARY KEY,
    channel               TEXT NOT NULL,
    target_ref            TEXT NOT NULL,
    state                 TEXT NOT NULL
        CHECK (state IN ('Opening', 'Active', 'Converging', 'Terminal',
                         'Reverted', 'Failed', 'Superseded', 'Pruned')),
    current_wave          INTEGER NOT NULL DEFAULT 0,
    -- FK columns are NULL-able under the v0.2 derived-view shape (matches
    -- probe_failures per §6.1 item 3 + RFC-0007 §7.2): the bounded-mpsc
    -- event_log writer is fire-and-forget so the applier doesn't know
    -- `seq` at co-write time. A follow-up tightens these to NOT NULL when
    -- the writer gains synchronous seq return.
    opened_event_log_seq  INTEGER REFERENCES event_log(seq),
    last_transition_event_log_seq INTEGER REFERENCES event_log(seq),
    opened_at             TEXT NOT NULL,
    terminal_at           TEXT,
    superseded_at         TEXT
);

CREATE INDEX rollouts_channel_state ON rollouts(channel, state);
CREATE INDEX rollouts_in_flight     ON rollouts(state)
    WHERE state IN ('Opening', 'Active', 'Converging', 'Reverted', 'Failed');
```

Every state column update carries a corresponding `event_log` row whose `seq` becomes the new `last_transition_event_log_seq`. The boolean methods (`is_superseded`, `is_terminal`, `is_finished`) collapse into a single `state` enum read.

### 6.4 `quarantined_closures` migration

New schema:

```sql
CREATE TABLE quarantined_closures (
    channel              TEXT NOT NULL,
    closure_hash         TEXT NOT NULL,
    quarantined_at       TEXT NOT NULL,
    -- NULL-able under the v0.2 derived-view shape; tightens to NOT NULL
    -- with the same writer-side change as rollouts + probe_failures.
    -- See §6.1 item 3.
    triggering_event_log_seq INTEGER REFERENCES event_log(seq),
    PRIMARY KEY (channel, closure_hash)
);

CREATE INDEX quarantined_closures_active ON quarantined_closures(channel);
```

The `triggering_event_log_seq` points at the `RollbackComplete` event (RFC-0005 §4.2) that produced the quarantine. Re-derivability: walk `event_log` for `RollbackComplete` events, group by `(channel, target_closure_hash)`, write one row per group with the lowest seq as the trigger.

## 7. Reducer composition

The rollout reducer and the host reducer both consume per-host events but with different concerns:

```
agent posts ProbeResult
    │
    ▼
applier receives event
    │
    ├─▶ host reducer: step(host_state, event, now) → (new_host_state, host_effects)
    │       │
    │       └─▶ applier writes event_log + probe_failures + host_rollout_records
    │
    └─▶ rollout reducer: step(rollout_state, RolloutEvent::HostStateChanged{...}, now)
            │                              → (new_rollout_state, rollout_effects)
            │
            └─▶ applier writes event_log (kind='rollout_event') + rollouts derived view
```

Both run in the same applier transaction. No new MPSC; no second mutator. The host reducer's output is the rollout reducer's input. Order is deterministic (host first, then rollout aggregates).

The two reducers remain in `nixfleet-state-machine`:

```
crates/nixfleet-state-machine/src/
  lib.rs                  — exports both step() functions
  host/                   — existing per-host reducer (RFC-0005 §3)
    state.rs, event.rs, effect.rs, transitions/...
  rollout/                — NEW per-rollout reducer (RFC-0008 §3)
    state.rs, event.rs, effect.rs, transitions/...
```

`Cargo.toml` purity contract unchanged: no `tokio`, no `reqwest`, no `rusqlite`, no `chrono::Utc::now()`. Both reducers are pure functions of their inputs.

## 8. Operator-visible improvements

- **`/v1/rollouts/{id}/events`** (RFC-0007 §7.2) becomes richer: it now surfaces rollout-level transitions in addition to per-host events. Operators see the full chronological story.
- **`/v1/rollouts`** (existing): can project rollout state from the new `state` enum column instead of computing it from booleans. The query simplifies.
- **Audit replay**: an auditor walking `event_log` chronologically reconstructs rollout-level state evolution without needing CP-internal knowledge. Today they would need to know that `record_active_rollout` SQL writes correspond to "rollout opened" — opaque.
- **No silent shadow-state drift**: by construction, `rollouts` and `quarantined_closures` can't disagree with `event_log` — they're written in the same transaction with FK-back.

