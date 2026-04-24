# RFC-0002: Rollout execution engine

**Status.** Draft.
**Depends on.** RFC-0001 (`fleet.nix` schema), nixfleet #2 (magic rollback), nixfleet #4 (compliance gates).
**Scope.** The decision procedure that turns `fleet.resolved` + observed fleet state into wave-by-wave reconciliation actions. Does not cover *how* actions reach hosts — that's RFC-0003.

## 1. Motivation

Once a fleet is declaratively resolved (RFC-0001), something has to decide: "given this desired state and what I see on the ground right now, what do I do next?" That's the reconciler. It must be deterministic, idempotent, observable, and provably safe under partial-visibility — hosts go offline, agents crash mid-activation, compliance probes fail, network partitions happen.

This RFC specifies its state machines and decision procedure. Implementation language is incidental (today: Rust on the control plane).

## 2. Inputs & outputs

**Inputs, read each reconcile tick:**

- `fleet.resolved` — the desired state JSON from RFC-0001.
- `channel refs` — current git ref per channel (from issue #3).
- `observed state` — per-host {current generation hash, last check-in timestamp, last reported health, last compliance probe result, current rollout membership}.
- `rollout history` — active and recently completed rollouts with their state.

**Outputs, emitted per reconcile tick:**

- Zero or more *intent updates* per host: "host X, target generation Y, within rollout R, wave W".
- Zero or more *rollout state transitions*: "rollout R wave W → Soaking", "rollout R → Halted".
- Zero or more *events* for observability: decisions, skips, waits, with structured reasoning.

The reconciler itself is stateless: all state lives in the database. A cold-started reconciler picking up an in-progress rollout converges to the same actions as the one that started it. This is essential for restarts and for future HA.

## 3. State machines

### 3.1 Rollout lifecycle

```
          Pending
             │
             │  (compliance static gate passes + release closure available)
             ▼
         Planning  ──(waves computed from policy + fleet.resolved)──▶  Executing
                                                                         │
                                       ┌─────────────────────────────────┤
                                       │                                 │
                                       ▼                                 ▼
                                  WaveActive                      (every wave done)
                                       │                                 │
                                 (in-flight hosts                        ▼
                                  reach Healthy                     Converged
                                  within wave budget)
                                       │
                                       ▼
                                 WaveSoaking
                                       │
                                 (soakMinutes elapsed
                                  + healthGate passes)
                                       │
                                       ▼
                                 WavePromoted ───▶ (next wave) ───▶ WaveActive
                                       │
                                    (last wave)
                                       │
                                       ▼
                                  Converged

                Failure branches from any WaveActive/WaveSoaking state:
                  ├─ onHealthFailure = "rollback-and-halt" → Reverting → Reverted
                  ├─ onHealthFailure = "halt"              → Halted
                  └─ operator override                      → Cancelled
```

Transitions are only taken during reconcile ticks. There is no async callback from an agent that directly mutates rollout state — agents update *observed state* only; the reconciler reads observed state and decides.

### 3.2 Per-host rollout participation

Within an active rollout, each member host has its own state:

```
  Queued ──▶ Dispatched ──▶ Activating ──▶ ConfirmWindow ──▶ Healthy ──▶ Soaked ──▶ Converged
                                              │
                                              │  (magic rollback triggered —
                                              │   host did not phone home)
                                              ▼
                                          Reverted
                                              │
                                              ▼
                                           Failed
```

- **Dispatched.** Control plane has set host's intent to new target generation. Host may still be offline.
- **Activating.** Agent has pulled the target and is running `nixos-rebuild switch`.
- **ConfirmWindow.** New generation booted; agent must phone home within the window (nixfleet #2, RFC-0003 §4.3).
- **Healthy.** Phone-home received; health gate evaluation begins.
- **Soaked.** Host has remained Healthy for `soakMinutes`.
- **Converged.** Wave promoted.
- **Reverted/Failed.** Either magic rollback fired, or health gate failed, or runtime compliance probe failed.

## 4. Decision procedure

On each reconcile tick (periodic: default 30s; event-triggered: on agent check-in, on git ref change, on manual nudge):

```
1.  Load fleet.resolved, observed state, active rollouts.
2.  For each channel c:
      a. If channels[c].ref differs from lastRolledRef[c]:
         → open a new rollout R for channel c at ref r.
         → static compliance gate:
              evaluate all type ∈ {static, both} controls against
              fleet.resolved[c].hosts configurations.
              If any required control fails → R ends in Failed (blocked).
         → Else → R.state = Planning.
3.  For each rollout R in Planning:
      a. Compute waves from policy.waves + selectors against current hosts.
      b. R.state = Executing; first wave → WaveActive.
4.  For each rollout R in Executing:
      a. For each wave W in R.currentWave:
           - If W is WaveActive:
               * For each host h in W with state ∈ {Queued, Dispatched} and
                 (h is online) and (no edge predecessor is incomplete) and
                 (disruption budgets permit):
                   → advance h to Dispatched, emit intent for h.
               * For hosts h ∈ W in ConfirmWindow:
                   → if deadline passed with no phone-home → h → Reverted.
               * For hosts h ∈ W in Healthy:
                   → evaluate health gate; if fail → h → Failed.
               * If all hosts in W are Soaked → W → WaveSoaking.
               * If failed-host count in W exceeds policy.healthGate.maxFailures:
                   → trigger policy.onHealthFailure.
           - If W is WaveSoaking:
               * If soak elapsed and runtime compliance probes pass for all
                 hosts in W → W → WavePromoted, advance R.currentWave.
5.  Emit events for every state transition with reasoning.
6.  Persist new state; commit atomically.
```

### 4.1 Edge ordering

Edges (RFC-0001 §2.5) are consulted *within the current wave*: a host cannot advance from Queued to Dispatched while any of its declared predecessors in the same rollout is not yet Converged. Edges across channels or across rollouts are ignored (edges are rollout-local; cross-rollout coordination is an explicit non-goal of v1).

### 4.2 Disruption budgets

Budgets (RFC-0001 §2.6) apply *across all active rollouts simultaneously*. A host counts against its budget from Dispatched through Converged. If advancing the next host would exceed `maxInFlight` or `maxInFlightPct` for any matching budget, the reconciler defers — host stays in Queued until a slot opens.

Budget evaluation is fleet-wide, not per-rollout. Two concurrent rollouts on different channels respect the same etcd budget.

### 4.3 Concurrency across channels

Channels roll out independently. A new rev on channel `edge-slow` can progress while `stable` is mid-rollout. The only global coordination is via disruption budgets.

Per-channel: at most one active rollout. A new ref arriving while a rollout is in progress is queued; when the current rollout reaches Converged / Halted / Cancelled, the queued ref triggers a fresh rollout. Queue depth ≤ 1 — if two new refs arrive, only the latest is retained (intermediate commits are skipped).

## 5. Failure handling

### 5.1 `onHealthFailure` semantics

- **`halt`** — freeze the rollout. Hosts already Converged stay on the new generation. In-flight hosts complete their current state transition naturally (no forced rollback). Operator must `nixfleet rollout {resume, cancel, rollback}`.
- **`rollback-and-halt`** — for every host in the rollout in state ∈ {Dispatched, Activating, ConfirmWindow, Healthy, Soaked, Converged}, emit intent to revert to the previous channel rev. Rollout ends in Reverted.
- **`rollback-all`** (future, out of scope for v1) — as above, and continue to revert hosts from *prior converged rollouts* on the same channel up to N generations back. Dangerous. Explicit opt-in.

### 5.2 Offline hosts

A host offline when its wave begins stays Queued indefinitely. Does not block wave progression — the wave advances once all *online* member hosts are Soaked, and the offline host is marked Skipped. When it returns, it is dispatched with the target of whatever the current channel ref is (not necessarily the one that was rolling out when it was offline).

Rationale: a laptop closed for two weeks should not block a fleet rollout, and should wake up to the *current* desired state, not replay history.

### 5.3 Probe failure taxonomy

Runtime compliance probes distinguish three outcomes (per the compliance RFC):

- **`passed`** — host advances.
- **`failed`** — host Failed; triggers `onHealthFailure`.
- **`probe-error`** — probe itself broken (nonzero exit, malformed output, timeout). Treated as failed unless `channel.compliance.strict = false`, in which case it's a warning and the host advances. Default strict.

## 6. Reconcile triggers

- **Periodic.** Default 30s. Tunable per-channel via `reconcileIntervalMinutes` (RFC-0001 §2.3) for slow channels like `edge-slow`.
- **Event-driven.**
  - Agent check-in with status delta → reconcile tick within ≤1s.
  - Git ref change (webhook or poll) → immediate tick.
  - Operator CLI command (`deploy`, `rollout cancel`, etc.) → immediate tick.

Debouncing: multiple events arriving within a small window (configurable, default 500ms) collapse to a single tick. Avoids thrashing under high check-in rates.

## 7. Observability

Every decision writes a structured event:

```json
{
  "ts": "2026-04-24T10:17:03Z",
  "rollout": "stable@abc123",
  "wave": 2,
  "host": "m70q-attic",
  "transition": "Queued → Dispatched",
  "reason": "edge predecessor db-primary reached Converged",
  "budgets": { "etcd": "not-applicable", "always-on": "3/10 in flight" }
}
```

Events are queryable via CLI (`nixfleet rollout trace <id>`) and emitted as structured logs. Every skip, every wait, every failure carries its reasoning — "why didn't this host upgrade yet?" must always be answerable from logs alone.

## 8. Open questions

1. **Re-entry when a host returns from offline.** Should the late-arriving host receive the *current* channel ref (skipping intermediate) or be replayed through the sequence of rollouts it missed? Lean: current only. Replaying violates "declarative" — the desired state is always "latest channel ref", history is noise.
2. **Per-channel rollout queue depth.** Should operators be able to set depth > 1 (keep every commit) or force coalescing (only latest)? Lean: coalesce always. Preserving every commit as a separate rollout invites a backlog and contradicts GitOps semantics where HEAD is truth.
3. **Cross-channel edges.** Genuinely useful for e.g. "database channel must finish before app channel starts". Deferred to v2; the workaround is putting both in the same channel.
4. **Scheduler fairness.** With many concurrent channels contending for the same disruption budget, should we use FIFO, priority, or fair-share? Lean: FIFO on rollout start time; revisit when anyone actually runs enough channels to care.
