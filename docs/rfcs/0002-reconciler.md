# RFC-0002: Rollout execution engine

**Status.** Accepted.
**Depends on.** RFC-0001 (`fleet.nix` schema), magic rollback, compliance gates.
**Scope.** The decision procedure that turns `fleet.resolved` + observed fleet state into wave-by-wave reconciliation actions. Does not cover *how* actions reach hosts — that's RFC-0003. The per-host state machine is RFC-0005; the rollout-level state machine is RFC-0008.

## 1. Motivation

Once a fleet is declaratively resolved (RFC-0001), something has to decide: "given this desired state and what I see on the ground right now, what do I do next?" That's the reconciler. It must be deterministic, idempotent, observable, and provably safe under partial-visibility - hosts go offline, agents crash mid-activation, compliance probes fail, network partitions happen.

This RFC specifies its state machines and decision procedure. Implementation language is incidental (today: Rust on the control plane).

## 2. Inputs & outputs

**Inputs, read each reconcile tick:**

- `fleet.resolved` - the desired state JSON from RFC-0001. **Signature-verified** against the pinned CI release key (RFC-0001 §4.3) before any field is read. A failed verification aborts the tick and raises an alert; the previously-verified `fleet.resolved` stays authoritative. Signatures that verify but predate `channel.freshnessWindow` (minutes, per-channel; RFC-0001 §2.3) are likewise rejected, preventing a compromised control plane from replaying old intent.
- `channel refs` - current git ref per channel (from issue #3).
- `observed state` - per-host {current generation hash, last check-in timestamp, last reported health, last compliance probe result, current rollout membership}.
- `rollout history` - active and recently completed rollouts with their state.

**Outputs, emitted per reconcile tick:**

- Zero or more *intent updates* per host: "host X, target generation Y, within rollout R, wave W".
- Zero or more *rollout state transitions*: "rollout R wave W -> Soaking", "rollout R -> Halted".
- Zero or more *events* for observability: decisions, skips, waits, with structured reasoning.

The reconciler itself is stateless: all state lives in the database. A cold-started reconciler picking up an in-progress rollout converges to the same actions as the one that started it. This is essential for restarts and for future HA.

## 3. State machines

### 3.1 Rollout lifecycle

The rollout-level state machine is defined by RFC-0008 §3 (8 states: `Opening → Active → Converging → Terminal`, with `Reverted` / `Failed` / `Superseded` / `Pruned` for failure and end-of-life). The v0.1 outline this section used to carry (Pending → Planning → Executing → WaveActive → WaveSoaking → WavePromoted → Converged) was superseded — see RFC-0008 for the canonical shape and the per-rollout `event_log` projection that records it.

Transitions are only taken during reconcile ticks. There is no async callback from an agent that directly mutates rollout state — agents update *observed state* only; the reconciler reads observed state and decides.

### 3.2 Per-host rollout participation

The per-host state machine inside a rollout is defined by RFC-0005 §3 (7 states: `Pending → Activating → Soaking → Converged`, with `Failed → Reverted` on sustained probe failure and `Deferred` for activations that staged the profile but skipped the live switch because dbus/systemd/kernel/init can't be hot-swapped). The v0.1 outline (Queued/Dispatched/ConfirmWindow/Healthy/Soaked) was superseded — see RFC-0005 for the canonical shape.

## 4. Decision procedure

On each reconcile tick (periodic: default 30s; event-triggered: on agent check-in, on git ref change, on manual nudge):

```
0.  Fetch fleet.resolved + signature; verify signature against pinned CI
    release key; reject if signature invalid OR
    (now − meta.signedAt) > channel.freshnessWindow  (minutes; per-channel,
    no default - RFC-0001 §2.3). On rejection: abort tick, keep
    last-verified snapshot, emit alert.
1.  Load verified fleet.resolved, observed state, active rollouts.
2.  For each channel c:
      a. If channels[c].ref differs from lastRolledRef[c]:
         -> open a new rollout R for channel c at ref r.
         -> static compliance gate:
              evaluate all type ∈ {static, both} controls against
              fleet.resolved[c].hosts configurations.
              If any required control fails -> R ends in Failed (blocked).
         -> Else -> R.state = Planning.
3.  For each rollout R in Planning:
      a. Compute waves from policy.waves + selectors against current hosts.
      b. R.state = Executing; first wave -> WaveActive.
4.  For each rollout R in Executing:
      a. For each wave W in R.currentWave:
           - If W is WaveActive:
               * For each host h in W with state ∈ {Queued, Dispatched} and
                 (h is online) and (no edge predecessor is incomplete) and
                 (disruption budgets permit):
                   -> advance h to Dispatched, emit intent for h.
               * For hosts h ∈ W in ConfirmWindow:
                   -> if deadline passed with no phone-home -> h -> Reverted.
               * For hosts h ∈ W in Healthy:
                   -> evaluate health gate; if fail -> h -> Failed.
               * If all hosts in W are Soaked -> W -> WaveSoaking.
               * If failed-host count in W exceeds policy.healthGate.maxFailures:
                   -> trigger policy.onHealthFailure.
           - If W is WaveSoaking:
               * If soak elapsed and runtime compliance probes pass for all
                 hosts in W -> W -> WavePromoted, advance R.currentWave.
5.  Emit events for every state transition with reasoning.
6.  Persist new state; commit atomically.
```

### 4.1 Edge ordering

Edges (RFC-0001 §2.5) are consulted *within the current wave*: a host cannot advance from Queued to Dispatched while any of its declared predecessors in the same rollout is not yet Converged. Edges across channels or across rollouts are ignored (edges are rollout-local; cross-rollout coordination is an explicit non-goal of v1).

### 4.2 Disruption budgets

Budgets (RFC-0001 §2.6) apply *across all active rollouts simultaneously*. A host counts against its budget from Dispatched through Converged. If advancing the next host would exceed `maxInFlight` or `maxInFlightPct` for any matching budget, the reconciler defers - host stays in Queued until a slot opens.

**In-flight definition.** A host is "in flight" for disruption-budget accounting if and only if its state is `Activating` or `Soaking` per RFC-0005 §3. `Pending` is NOT in flight: the host has not yet been dispatched and continues serving on the prior closure. Exit states (`Converged`, `Failed`, `Reverted`) are not in flight. The planner additionally counts its own `QueueDispatch` emissions within a single `plan_next()` invocation against the budget, so within-tick batches respect `maxInFlight` even though the underlying `fleet_state` snapshot does not refresh between gate evaluations.

**Budget snapshots are per-rollout; identity is by selector.** Each rollout's manifest carries a frozen `disruption_budgets[]` snapshot - the operator's selector resolved against `fleet.hosts` at OpenRollout time. The reconciler reads the snapshot, never the live `fleet.disruptionBudgets[].selector`. Mid-rollout retags therefore cannot reshape an in-flight rollout's budget membership; they take effect on the next rollout. Cross-rollout fleet-wide enforcement survives the snapshot model: in-flight summing matches budgets across active rollouts by selector equality, so two rollouts that share a `tags = ["etcd"]` budget cap concurrent etcd disruption to the global maxInFlight.

### 4.3 Concurrency across channels

Two ordering primitives, in increasing strictness:

1. **Disruption budgets** - fleet-wide caps on in-flight count. Always active. Two channels rolling out concurrently respect the same `tags = ["etcd"]` cap.

2. **`channelEdges`** - DAG ordering between channels. A `{ before; after }` edge holds OpenRollout for `after` until `before` has no non-terminal rollout. This is the v0.3 punt closed: cross-channel coordination is no longer "punt to disruption budgets only", it has its own primitive. Edge predecessors with no rollout history are open (proceed); a `Halted` predecessor blocks `after` until the operator resolves it. The reconciler emits `Action::RolloutDeferred { channel, target_ref, blocked_by, reason }` when the edge holds; emission is debounced via `Observed.last_deferrals` so a still-blocked channel doesn't pollute the journal across reconcile ticks.

Per-channel: at most one active rollout. A new ref arriving while a rollout is in progress is queued; when the current rollout reaches Converged / Halted / Cancelled, the queued ref triggers a fresh rollout. Queue depth ≤ 1 - if two new refs arrive, only the latest is retained (intermediate commits are skipped).

### 4.4 Rollout manifests

A `RolloutManifest` is the per-rollout signed plan: the frozen view of which hosts are in which wave, signed by CI at the same time it signs `fleet.resolved.json`. The manifest is the artifact that lets agents verify their wave assignment without trusting the CP.

**Why this exists.** `fleet.resolved.json` is the desired-state snapshot - it rolls forward continuously as new CI commits land. A rollout has a different temporal scope: its plan freezes at rollout-open and stays frozen until the rollout terminates. Without a separately-named, frozen artifact, an attacker (or buggy CP) could serve host A "you're in wave 1" and host B "you're in wave 3" of the same logical rollout, and neither agent could detect the inconsistency. Content-addressing the manifest closes that gap.

**Producer.** CI produces *N+1 signed artifacts per commit* where N is the number of channels in `fleet.resolved.channels`: the resolved snapshot itself, plus one manifest per channel. Each manifest is a deterministic projection of `fleet.resolved` for one channel - every input (host membership, wave layout, target closure, health gate, compliance frameworks) is already inside the signed snapshot. CI signs both with the same `ciReleaseKey`. The CP holds no signing key for rollouts; it is a verified stateless distributor.

**Identifier.** `rolloutId = "{channel}@{channel_ref}"`, constructed via `RolloutId::new(channel, channel_ref)` (RFC-0008 §6.3). Two channels can share a `channel_ref` (the architectural point of multi-channel cascading from a single git push), so the composite is the canonical key. Verification recomputes the identifier from the parsed manifest fields and compares against the advertised value; mismatch is a hard refuse-to-act.

**Anchor.** The manifest carries `fleetResolvedHash` - sha256 of the canonical bytes of the `fleet.resolved.json` it was projected from. This closes a mix-and-match attack: during a key-rotation overlap window where both predecessor and successor sign valid `fleet.resolved.json` snapshots at the same channel ref, an attacker without the anchor could pair a manifest from snapshot X with the resolved.json from snapshot Y. The anchor makes that inconsistency provably detectable.

**Adoption.** When the reconciler opens a new rollout for channel `c` at ref `r` (step 2a of the reconcile loop), it loads `releases/rollouts/<rolloutId>.json`, verifies the signature against `ciReleaseKey`, recomputes the content hash, and persists `(rollout_id, manifest_hash, host_set)` into `host_rollout_state`. **If the manifest is missing or fails verification, the CP refuses to open the rollout.** There is no fallback path to unsigned dispatch - the inversion-of-trust property does not bend.

**Distribution.** Agents fetch the manifest via `GET /v1/rollouts/<rolloutId>` (RFC-0003 §4.6) on first sight, verify it independently against the trust roots they already hold, recompute the hash, and assert that `(hostname, wave_index)` ∈ `manifest.host_set`. Mismatch is a hard refuse-to-act with `ReportEvent::ManifestMismatch`. The cached manifest is the source of truth for the rollout's lifetime - subsequent checkins re-assert that the CP-advertised `rolloutId` matches the cached one. A second-call manifest with the same `rolloutId` but different content cannot exist (the hash would differ).

**Schema.** Defined in `nixfleet-proto::rollout_manifest`. The `host_set` array MUST be sorted by `hostname` ascending; the per-budget `hosts` arrays in `disruption_budgets` MUST be sorted alphabetically. JCS sorts object keys but not array elements, so the producer's emission order is the canonical order.

**Disruption-budget snapshot.** Each manifest carries `disruption_budgets[]` - the operator's selectors from `fleet.disruptionBudgets` resolved against `fleet.hosts` at projection time, frozen for the rollout's life. The reconciler reads from this snapshot rather than re-resolving live `fleet.hosts.tags` per tick, which is what makes mid-rollout retag safe (§4.2). Cross-rollout in-flight counting matches budgets by selector equality.

**Future work.** With `len(host_set)` in the thousands, full-roster manifests grow into the hundreds of KB. Per-host scoping (one signed object per host) trades manifest count for message size; a Merkle-inclusion proof shape trades both at the cost of a more complex verifier. Single-tenant fleets at v0.2 scale do not need either; they belong in v0.3.

## 5. Failure handling

### 5.1 `onHealthFailure` semantics

- **`halt`** - freeze the rollout. Hosts already Converged stay on the new generation. In-flight hosts complete their current state transition naturally (no forced rollback). Operator must `nixfleet rollout {resume, cancel, rollback}`.
- **`rollback-and-halt`** - for every host in the rollout in state ∈ {Dispatched, Activating, ConfirmWindow, Healthy, Soaked, Converged}, emit intent to revert to the previous channel rev. Rollout ends in Reverted.
- **`rollback-all`** (future, out of scope for v1) - as above, and continue to revert hosts from *prior converged rollouts* on the same channel up to N generations back. Dangerous. Explicit opt-in.

### 5.2 Offline hosts

A host offline when its wave begins stays Queued indefinitely. Does not block wave progression - the wave advances once all *online* member hosts are Soaked, and the offline host is marked Skipped. When it returns, it is dispatched with the target of whatever the current channel ref is (not necessarily the one that was rolling out when it was offline).

Rationale: a laptop closed for two weeks should not block a fleet rollout, and should wake up to the *current* desired state, not replay history.

### 5.3 Probe failure taxonomy

Runtime compliance probes distinguish three outcomes (per the compliance RFC):

- **`passed`** - host advances.
- **`failed`** - host Failed; triggers `onHealthFailure`.
- **`probe-error`** — probe itself broken (nonzero exit, malformed output, timeout). Treated as `failed` uniformly (RFC-0007 §6). Operators who want "tolerate probe errors" use per-probe `mode = "observe"` (RFC-0007 §3.3).

## 6. Reconcile triggers

- **Periodic.** Default 30s. Tunable per-channel via `reconcileIntervalMinutes` (RFC-0001 §2.3) for slow channels like `edge-slow`.
- **Event-driven.**
  - Agent check-in with status delta -> reconcile tick within ≤1s.
  - Git ref change (webhook or poll) -> immediate tick.
  - Operator CLI command (`deploy`, `rollout cancel`, etc.) -> immediate tick.

Debouncing: multiple events arriving within a small window (configurable, default 500ms) collapse to a single tick. Avoids thrashing under high check-in rates.

## 7. Observability

Every decision writes a structured event:

```json
{
  "ts": "2026-04-24T10:17:03Z",
  "rollout": "stable@abc123",
  "wave": 2,
  "host": "attic-01",
  "transition": "Queued -> Dispatched",
  "reason": "edge predecessor db-primary reached Converged",
  "budgets": { "etcd": "not-applicable", "always-on": "3/10 in flight" }
}
```

Events are queryable via CLI (`nixfleet rollout trace <id>`) and emitted as structured logs. Every skip, every wait, every failure carries its reasoning - "why didn't this host upgrade yet?" must always be answerable from logs alone.

