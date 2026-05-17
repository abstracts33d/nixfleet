# RFC-0008: Event-driven host-rollout state

**Status.** Draft (slated for v0.2 cycle; no compat layer — v0.2 is a full rewrite).
**Depends on.** RFC-0001 (fleet topology), RFC-0002 (reconciler), RFC-0003 (agent/CP protocol).
**Supersedes.** Sections 4 + 5 of RFC-0003 (polling-based checkin contract) — replaced wholesale, not extended.
**Scope.** Per-host per-rollout state machine and the wire vocabulary that drives it. Defines the explicit dispatch → ack → multi-stage report flow that replaces the inference-from-checkin model documented in v0.2.0 polish. Does not cover control-plane-internal reconciler logic, channel-level rollout opening, or signing — those stay as RFC-0002 / RFC-0003 specify them.
**Implementation.** New event variants in `crates/nixfleet-proto/src/agent_wire.rs` (replacing the old checkin-derived state machine in the same file). New `HostRolloutRecord` schema in `crates/nixfleet-control-plane/src/db/` (replaces `host_dispatch_state` + `host_rollout_state` rows). Per-event handlers in `crates/nixfleet-control-plane/src/server/events.rs` (new module; the v0.2.0 `health-sweep` block in `reconcile.rs` is deleted). Agent activation pipeline rewrite in `crates/nixfleet-agent/src/activation/`.

## 1. Problem statement

The pre-v0.2 protocol (RFC-0003 §4.1) is **inference-driven**: the agent sends a periodic checkin every ~60 s carrying its current state snapshot (`currentClosureHash`, `pendingClosureHash`, `outstandingHealthFailures`, probe results). The control plane reconstructs state transitions by diffing successive snapshots and stamping its own timestamps from wallclock observations.

This produces a class of bugs that share one root cause — **CP guesses transitions it should be told about**. Validation work on the v0.2.0 demo (after the surgical fixes in `1a3e1f80` / `c3ab9d75` landed) surfaced six concrete instances:

1. **`current_closure_hash` lags ~60 s after rollback.** Agent fires `switch-to-configuration` on the prior closure, but CP doesn't learn the host is on the prior closure until the next regular checkin. During the gap, status shows `state = Reverted, current == declared == bad SHA`.
2. **Probe gate satisfied by stale `Pass` results.** Agent's `ProbeStateCache` is process-lifetime; activating a new closure does not reset it. CP sees `host_probes_observed = true && host_probes_passing = true` from the *previous* closure's probes and lets `Healthy → Soaked` fire before any probe has run against the new closure.
3. **Sweep threshold effectively `60 s + first_checkin_lag`.** CP's `first_seen` is wallclock when it noticed `outstandingHealthFailures > 0` (i.e., when a checkin reporting failures arrived), not when the failure actually started. Observed: 89 s end-to-end on a `HEALTH_FAILURE_THRESHOLD_SECS = 60` constant.
4. **Soak fires too eagerly with `soakMinutes = 0`.** `Healthy → Soaked` is reconcile-tick driven; with a zero soak window the transition can happen in the same tick as confirm-ack, before any probe has actually run.
5. **Channel-edge gate over-holds.** Predecessor's no-op rollout (closure unchanged) leaves `host_states` empty in `RolloutDbSnapshot`. `is_active_for_ordering()` returned `true` on empty until `terminal_at` was honored (`c3ab9d75`, v0.2 polish). The fix worked but is symptomatic — CP shouldn't have to *infer* "predecessor done" from an absence.
6. **State shapes that the schema permits but the operator cannot interpret.** `rolloutState = Soaked, current != declared` is a real combination CP can produce, but it's nonsensical operationally. The CLI papers over it with conditional labels (`✗ failed` vs `→ reverting`) — five lines of view-layer logic to mask one model defect.

All six are the same disease: **transitions happen, but CP is the last to know.**

## 2. Design goals

1. **Every state transition is event-driven.** CP changes state on receipt of an explicit agent event, never on the diff of two checkins.
2. **The agent owns the timestamp of every transition.** CP stores the agent's reported `at` field, not wallclock-on-receipt. Sweep windows, soak windows, gate eligibility — all from agent-supplied timestamps.
3. **Probe state is per-rollout, not per-process.** Each `ActivationComplete` event resets the probe cache for the new rollout. Stale results from the prior closure cannot satisfy the new rollout's gates.
4. **Polling becomes a fallback, not the primary channel.** Long-poll for the inbound `Dispatch` direction (the only queued message — rollback is agent-decided per §2.1); explicit event reports for outbound state. The 60 s heartbeat remains as a liveness signal and a missed-event drift detector, not as the source-of-truth.
5. **No CLI conditionals for impossible states.** The new state machine forbids the shapes (`Soaked, current != declared`, `Failed, current != declared`) that exist today only because the model is loose.
6. **No legacy code paths.** v0.2 is a fresh wire revision; the pre-v0.2 checkin-as-state-source path is deleted, not preserved. Both agent and CP ship event-driven from day one.

### 2.1 Trust model alignment (RFCs 0001–0007 invariants this RFC preserves)

The event-driven model **does not** alter the trust contract established by the prior RFCs. Specifically:

- **CP holds no signing key** (RFC-0002 §3). Every agent event is signed by the agent's mTLS client cert (RFC-0003 §2). CP signs nothing it emits. **Amendment (2026-05-17):** CP does hold a **CA-issuance** signing key for `/v1/enroll` and `/v1/agent/renew-cert`, introduced after this RFC was written. Production deployments bind this key to the TPM so CP holds only a pubkey + sign-wrapper handle (no in-memory key material), preserving the spirit of the claim. The file-backed fallback violates it. See RFC-0005 §1.5.1 for the precise contract and the `--strict` enforcement that makes the TPM-only deployment mechanical.
- **CP holds no trust private keys** (RFC-0005 §1.5). Verification of inbound events uses the same `TrustConfig` deserialized at CP boot — no new trust roots, no new secrets. (The CA-issuance key is not a `TrustConfig` entry; see RFC-0005 §1.5.1 for the signer-vs-verifier distinction.)
- **CP is reconstructible from git + agent state** (RFC-0001 §10, RFC-0005 §1.5). The `HostRolloutRecord` table (§5) is a **cache**, not a source of truth. On CP rebuild (loss of `/var/lib/nixfleet-cp/state.db`), the heartbeat drift-detection in §4.3 prompts agents to replay their event log; CP rebuilds its view from agent reports. Historical timestamps for *converged* rollouts are lost on rebuild — same property as today's DB.
- **Inversion of trust is preserved** (RFC-0002 §4, RFC-0003 §4.6). The `Dispatch` event's `target_closure` field is **advisory** — a convenience pointer to the canonical value in the signed manifest the agent already holds. Agents MUST verify the field against `manifest.host_set[hostname].target` and refuse-to-act on mismatch. CP cannot redirect an agent to an unsigned closure by tampering with the `Dispatch` payload; mTLS protects the wire, and the manifest signature catches any substitution. **Rollback decisions are made by the agent directly from the signed manifest's `onHealthFailure` policy** — CP issues no `RollbackSignal`. Net effect: there is exactly one signed source of truth for every action an agent takes, and CP cannot direct the agent off that source.
- **Pull-only control flow** (RFC-0003 §1). CP never reaches an agent. The only queued message is `Dispatch` (a wave-timing signal); the agent fetches it on its next long-poll to `/v1/agent/dispatch`. Rollback is agent-decided, so no rollback message is ever queued. The wording "CP issues" anywhere this RFC uses it is shorthand for "CP queues for agent retrieval"; no socket is opened in the CP→agent direction.
- **CP blast radius unchanged** (RFC-0005 §1.5). CP holds no new secrets, no new trust authority. SSH access to the CP host remains equivalent to SSH access to any production NixOS box.
- **Air-gap operation unaffected** (RFC-0007). Events are signed payloads on a request/response wire; they ride sovereign caches the same way checkins do today. The CP-side event handler is identical online and air-gapped.

## 3. State machine

```
                         ┌─────────────────────────────────────────┐
                         ▼                                         │
   ┌─────────┐     ┌────────────┐    ┌───────────┐    ┌────────────┐
   │ Pending │────▶│ Activating │───▶│  Soaking  │───▶│ Converged  │
   └─────────┘     └────────────┘    └───────────┘    └────────────┘
                         │                 │
                         │                 │ sustained probe fail
                         ▼                 ▼
                   ┌───────────┐    ┌────────────┐
                   │  Failed   │───▶│  Reverted  │
                   └───────────┘    └────────────┘
                                          │
                                          │ channel halt-lift on
                                          │ new declared SHA
                                          ▼
                                    (new rollout → Pending)
```

**Six states, no aliases:**

| State | Meaning | Entered by | Exited by |
|---|---|---|---|
| `Pending` | CP has issued a Dispatch; agent has not yet ACKed (or rollout was just opened) | `Dispatch` issued | `DispatchAck` received |
| `Activating` | Agent acked; switch-to-configuration is firing or has fired pending confirmation | `DispatchAck` | `ActivationComplete` (success path) or `ActivationFailed` |
| `Soaking` | Agent reports activation succeeded; probes have started running against the new closure; soak window has not yet elapsed | `ActivationComplete` | `Converged` event or `Failed` (via sweep) |
| `Converged` | Soak elapsed, probes passing, `current == declared`. **Terminal for ordering.** | `Converged` event from agent | New rollout opens for this channel |
| `Failed` | Sustained probe failure observed by the agent and reported to CP. Agent has read `onHealthFailure` from the signed manifest and decided autonomously what comes next. | `Failed` event (agent reports sustained failure) | `RollbackComplete` (if policy was `rollback-and-halt`) or operator action (if `halt-only`) |
| `Reverted` | Agent has completed rollback to prior closure. Channel-level quarantine holds the bad SHA. | `RollbackComplete` | Channel publishes a new SHA (declared moves past quarantine) |

**States explicitly removed vs. RFC-0003 / today's enum:**

- `Queued` — collapsed into `Pending`.
- `Dispatched` — was a CP-side bookkeeping flag, not a host state; replaced by `Pending` with a `dispatched_at` timestamp.
- `ConfirmWindow` — `Activating` covers it; `ActivationComplete` ends the phase.
- `Healthy` — collapsed into `Soaking`; the "Healthy means probes report but soak window not elapsed" distinction was internal bookkeeping.
- `Soaked` (separate from `Converged`) — terminal state was bifurcated by rollout policy (`Soaked` for canary, `Converged` for all-at-once). Now both end at `Converged`. Soak duration just affects when you reach it; the destination is the same.

**State invariants that the schema now enforces:**

- `Converged` ⇒ `current == declared && all_enforce_mode_probes == Pass` (per RFC-0010 §3.3; observe and disabled probes do not gate). CP refuses to write this state otherwise.
- `Reverted` ⇒ `current != declared && current == reverted_to`. Same enforcement.
- `Failed` does NOT imply anything about `current` vs `declared` — it means "we observed sustained failure on the dispatched target"; the agent may not have started rollback yet.

## 4. Event vocabulary

All events are signed by the agent's mTLS client cert (already RFC-0003 §2). Event payloads are JSON, canonicalised per RFC-0003 §3. Every event carries `rollout_id`, `hostname`, and `seq` (monotonic per `(hostname, rollout_id)` pair; gaps signal lost events; out-of-order events are dropped with a warning).

> **Amendment (2026-05-17) — `rollout_id` canonical wire format.** The JSON
> examples throughout §4.1, §4.2, and §4.3 show `"rollout_id": "<uuid>"` as a
> generic placeholder. The canonical wire format is
> `"{channel}@{channel_ref}"` (e.g., `"stable@a1b2c3d4"`), per RFC-0012 §6.3.
> The identifier is constructed via `RolloutId::new(channel, channel_ref)` in
> `crates/nixfleet-proto/src/lib.rs` (newtype with no public raw constructor;
> test-only escape hatch under `#[cfg(any(test, feature = "test-helpers"))]`,
> same discipline as `Verified<T>` per RFC-0009 §3). UUIDs are never emitted
> on the wire; CP-side validation enforces the canonical shape via the
> `looks_like_rollout_id` route filter and the reducer's
> `RolloutId`-discriminated supersession check. Rationale for the composite
> over either half: two channels can share a `channel_ref` (the architectural
> point of multi-channel cascading from a single git push), and
> `rollout_id = channel_ref` alone collides in that topology while
> `rollout_id = channel` alone violates the content-addressed property of the
> rest of the cycle — see RFC-0012 §6.3.

### 4.1 Queued for agent retrieval (agent long-polls `/v1/agent/dispatch`)

Per RFC-0003 §1 (pull-only control flow), CP never opens a connection to an agent. The agent long-polls `/v1/agent/dispatch`; when CP has queued a `Dispatch` for that host (the only queued message — see §2.1 for why no rollback message is queued), the response carries it. Otherwise the request blocks up to the long-poll timeout (default 60 s) and returns empty.

#### `Dispatch`

CP queues this when a host is up for activation under the current rollout. **The payload is advisory**: `target_closure` is a convenience pointer to the canonical value in the signed manifest the agent already fetched per RFC-0003 §4.6. The agent MUST verify `target_closure == manifest.host_set[hostname].target` before acting; mismatch is a hard refuse-to-act (emit `DispatchReject`, do not consume any other field).

```json
{
  "kind": "Dispatch",
  "rollout_id": "<uuid>",
  "target_closure": "<store-path-hash>",
  "channel": "stable",
  "wave": 0,
  "soak_due_at": "2026-05-16T01:30:00Z",
  "confirm_deadline": "2026-05-16T01:33:00Z",
  "issued_at": "2026-05-16T01:27:00Z",
  "seq": 1
}
```

Agent MUST respond with `DispatchAck` (after manifest cross-check) before starting the switch. If `target_closure` is in the agent's local quarantine (rare; CP also enforces), agent responds `DispatchReject` instead.

**There is no `RollbackSignal` queued by CP.** The agent reads `manifest.channels[<channel>].rollout_policy.onHealthFailure` directly from the signed manifest it already holds (RFC-0003 §4.6); when it self-detects sustained failure (§4.2 `Failed`), it acts on that policy autonomously — no CP round-trip. CP's role is to **record** that this happened, not to **decide** it. This collapses the rollback path into a single signed source of truth (the manifest) and removes any possibility of CP/agent disagreement on whether rollback should fire.

### 4.2 Outbound from agent (POST `/v1/agent/events`)

The agent sends one POST per event. CP returns `204 No Content` on success. On `4xx`, agent must NOT retry (event was rejected as invalid). On `5xx` / network failure, agent retries with exponential backoff and the same `seq`; CP deduplicates by `(hostname, rollout_id, seq)`.

#### `DispatchAck` — Pending → Activating

```json
{
  "kind": "DispatchAck",
  "rollout_id": "<uuid>",
  "received_at": "2026-05-16T01:27:01Z",
  "current_closure_at_dispatch": "<prior-closure-hash>",
  "seq": 2
}
```

CP transition: `Pending → Activating`. CP stores `current_closure_at_dispatch` as the canonical rollback target (do not re-derive from `/run/current-system` later — agent might restart, lose state).

#### `ActivationStarted` — visibility, no transition

```json
{
  "kind": "ActivationStarted",
  "rollout_id": "<uuid>",
  "started_at": "2026-05-16T01:27:03Z",
  "switch_method": "systemd-run-detached",
  "seq": 3
}
```

CP records timestamp. No state change. Used for operator observability (`status --rollout-history`).

#### `ActivationComplete` — Activating → Soaking

```json
{
  "kind": "ActivationComplete",
  "rollout_id": "<uuid>",
  "completed_at": "2026-05-16T01:27:05Z",
  "observed_current_closure": "<store-path-hash>",
  "switch_exit_code": 0,
  "seq": 4
}
```

CP transition: `Activating → Soaking`. CP also:
- Stamps `activation_completed_at = completed_at`.
- Sets `current_closure = observed_current_closure`.
- Resets the probe state for this `(hostname, rollout_id)` — any prior `ProbeResult` for this pair is invalidated.
- Records `soak_due_at` (from the original `Dispatch`).

#### `ActivationFailed` — Activating → Failed

```json
{
  "kind": "ActivationFailed",
  "rollout_id": "<uuid>",
  "failed_at": "2026-05-16T01:27:05Z",
  "switch_exit_code": 1,
  "stderr_tail": "<...truncated stderr...>",
  "seq": 4
}
```

CP transition: `Activating → Failed`. If the manifest's `onHealthFailure` is `rollback-and-halt`, the agent immediately fires the rollback on its own — same single signed source of truth as the §4.2 `Failed`-via-sustained-probe-fail path — and the next event from this agent for this rollout will be `RollbackComplete`.

#### `ProbeTopologyDeclared` — authoritative declared-probe set

```json
{
  "kind": "ProbeTopologyDeclared",
  "rollout_id": "<uuid>",
  "declared_at": "2026-05-16T01:27:15Z",
  "probes": [
    { "name": "nginx-version",   "kind": "http",     "mode": "enforce" },
    { "name": "heartbeat",       "kind": "http",     "mode": "observe" },
    { "name": "evidence-nis2",   "kind": "evidence", "mode": "enforce" }
  ],
  "seq": 4
}
```

Added in v0.2.1 (RFC-0010 §8.1). Emitted once per `ActivationCompleted` by the agent's probe worker after re-reading `/etc/nixfleet/agent/health-checks.json`. CP treats the payload as the authoritative set of probes the agent has committed to running for this rollout. Required for the wave-promotion gate to distinguish "enforce probe hasn't reported yet — hold" from "no enforce probes declared — advance." Missing this event holds the wave with reason `"awaiting probe topology"`.

#### `ProbeObservedFirst` — gates may now consult probes

```json
{
  "kind": "ProbeObservedFirst",
  "rollout_id": "<uuid>",
  "observed_at": "2026-05-16T01:27:20Z",
  "probe_name": "nginx-version",
  "mode": "enforce",
  "seq": 5
}
```

CP records `probe_observed_first_at` for this rollout. The soak gate (was `host_probes_observed`) consults THIS field, not a snapshot. Agents emit one per declared probe on first run after activation. The `mode` field (added in v0.2.1 per RFC-0010 §8.2) makes the event self-describing for replay — readers don't need to join against the topology declaration to interpret per-probe enforcement.

#### `ProbeResult`

```json
{
  "kind": "ProbeResult",
  "rollout_id": "<uuid>",
  "probe_name": "nginx-version",
  "status": "Pass" | "Fail" | "Unknown",
  "observed_at": "2026-05-16T01:27:20Z",
  "failure_reason": "<optional>",
  "mode": "enforce",
  "sub_results": null,
  "seq": 6
}
```

Streamed on each probe run. CP updates its per-rollout probe map. Note: `Unknown` is NOT reported — first result is always `Pass` or `Fail`. (Unknown is the bootstrap state before any run.)

**v0.2.1 additions** (RFC-0010 §7.1 + §8.2):

- `mode` (always present): one of `enforce | observe | disabled`. Self-describing for replay; redundant with the topology declaration but cheap (4 bytes) and removes a table join at gate-eval time.
- `sub_results` (`Option<Vec<ProbeSubResult>>`): populated for `kind = "evidence"` probes only; `None` for HTTP/TCP/exec. Each entry carries `{control_id, status, framework, article}` so operator dashboards preserve per-control failure visibility. Aggregate `status` is `Pass` iff every `sub_result.status == Pass`. The applier expands `sub_results` into per-control rows in the `probe_failures` derived view (RFC-0010 §7.2) within the same transaction that appends to `event_log`.

Probe-error semantics (uniform across all kinds per RFC-0010 §6): a probe that fails to execute (nonzero exit, malformed output, timeout) reports `status = "Fail"`. There is no probe-error-tolerance flag; operators who want "tolerate probe errors" use per-probe `mode = "observe"`.

#### `ProbeFailureFirst` — sweep starts ticking

```json
{
  "kind": "ProbeFailureFirst",
  "rollout_id": "<uuid>",
  "probe_name": "nginx-version",
  "first_failed_at": "2026-05-16T01:27:35Z",
  "seq": 7
}
```

Emitted by the agent on the first `Pass → Fail` transition (or first-ever `Fail`) for any declared probe. CP stamps `probe_failure_first_at` from `first_failed_at` (agent's timestamp). Sweep window now measured from this exact time, not from CP wallclock.

#### `Failed` — Soaking → Failed (sustained-failure self-report)

```json
{
  "kind": "Failed",
  "rollout_id": "<uuid>",
  "failed_at": "2026-05-16T01:28:35Z",
  "sustained_duration_secs": 60,
  "failing_probes": ["nginx-version"],
  "policy_applied": "rollback-and-halt",
  "seq": 12
}
```

The **agent** detects sustained failure (its own probe-cache crosses `HEALTH_FAILURE_THRESHOLD_SECS`), reads `onHealthFailure` from the signed manifest, and reports `Failed`. The `policy_applied` field records which manifest policy branch the agent is about to follow:
- `rollback-and-halt`: agent immediately fires the rollback (next event will be `RollbackComplete`).
- `halt-only`: agent stops and stays Failed; operator action required.

CP transitions `Soaking → Failed`. Note: the CP-side sweep (the legacy `health-sweep` block in `reconcile.rs`) is removed; the agent is the source of truth on its own probe state AND on the rollback decision.

#### `RollbackComplete` — Failed → Reverted

Emitted by the agent after it has autonomously executed the rollback (no CP signal required). The agent reads the rollback target from its own `current_closure_at_dispatch` (recorded at `DispatchAck` time per §4.2) and fires `switch-to-configuration` on that closure directly.

```json
{
  "kind": "RollbackComplete",
  "rollout_id": "<uuid>",
  "completed_at": "2026-05-16T01:28:40Z",
  "reverted_to_closure": "<prior-closure-hash>",
  "switch_exit_code": 0,
  "seq": 13
}
```

CP transition: `Failed → Reverted`. CP:
- Stamps `reverted_at = completed_at`.
- Sets `current_closure = reverted_to_closure`.
- Inserts the dispatched-but-bad `target_closure` into the channel's `quarantined_closures` set (already the v0.2.0 fix #6 mechanism — unchanged, just consumes the agent-reported event rather than CP-derived state).

#### `Converged` — Soaking → Converged

```json
{
  "kind": "Converged",
  "rollout_id": "<uuid>",
  "converged_at": "2026-05-16T01:30:05Z",
  "current_closure": "<store-path-hash>",
  "seq": 30
}
```

Agent emits this when:
- `soak_due_at` has elapsed,
- all declared probes are `Pass`,
- `current_closure == target_closure` from the `Dispatch`.

CP transitions `Soaking → Converged` after re-verifying the same three invariants on the server side from the recorded state. If any invariant fails, CP rejects the event (`409 Conflict`) and the agent retries after re-checking.

### 4.3 Heartbeat

`POST /v1/agent/heartbeat` (replaces RFC-0003's `/v1/agent/checkin`). Minimal payload:

```json
{
  "hostname": "web-01",
  "agent_version": "0.2.0",
  "current_closure": "<store-path-hash>",
  "uptime_secs": 3600,
  "last_event_seq_by_rollout": {
    "<rollout-id>": 14
  },
  "at": "2026-05-16T01:30:00Z"
}
```

Purpose:

- **Liveness.** CP marks agent reachable; missed heartbeats (3× interval) raise a `HostUnreachable` alert.
- **Drift detection.** If `current_closure` disagrees with CP's `HostRolloutRecord.current_closure` for the host's latest rollout, events were lost. CP responds `200 + X-Nixfleet-Replay-From: <seq>` listing the last `seq` it has per rollout; agent re-sends events with `seq > X` (deduplicated server-side by `(hostname, rollout_id, seq)`).
- **No state transitions.** Heartbeats never advance the state machine. State only changes on receipt of a §4.2 event.

Default interval: 60 s. Adjustable per fleet.

## 5. `HostRolloutRecord` schema

Replaces the current `host_dispatch_state` row + `host_rollout_state` row + scattered timestamps. One row per `(rollout_id, hostname)`:

```rust
pub struct HostRolloutRecord {
    pub rollout_id: RolloutId,
    pub hostname: String,
    pub channel: String,
    pub state: HostState,                              // 6-variant enum from §3

    // Closures
    pub target_closure: ClosureHash,                   // from Dispatch
    pub current_closure_at_dispatch: Option<ClosureHash>, // from DispatchAck
    pub current_closure: Option<ClosureHash>,          // from ActivationComplete / RollbackComplete
    pub reverted_to: Option<ClosureHash>,              // = current_closure_at_dispatch

    // Transition timestamps (all agent-supplied; CP never writes wallclock here)
    pub dispatched_at: DateTime<Utc>,                  // CP-issued; CP wallclock OK here
    pub dispatch_acked_at: Option<DateTime<Utc>>,      // from DispatchAck
    pub activation_started_at: Option<DateTime<Utc>>,  // from ActivationStarted
    pub activation_completed_at: Option<DateTime<Utc>>, // from ActivationComplete
    pub activation_failed_at: Option<DateTime<Utc>>,   // from ActivationFailed
    pub probe_observed_first_at: Option<DateTime<Utc>>, // from ProbeObservedFirst
    pub probe_failure_first_at: Option<DateTime<Utc>>, // from ProbeFailureFirst
    pub soak_due_at: Option<DateTime<Utc>>,            // computed at Dispatch issue
    pub converged_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub policy_applied: Option<RolloutFailurePolicy>,  // from Failed event (rollback-and-halt | halt-only)
    pub reverted_at: Option<DateTime<Utc>>,

    // Live probe state
    pub probes: HashMap<String, ProbeRecord>,          // by probe_name

    // Event ordering
    pub last_event_seq: u64,
}

pub struct ProbeRecord {
    pub status: ProbeStatus,           // Pass | Fail (never Unknown post-bootstrap)
    pub last_observed_at: DateTime<Utc>,
    pub last_pass_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}
```

## 6. Gate redesign (consult explicit fields)

Every gate in `crates/nixfleet-reconciler/src/gates/` becomes a pure function of the explicit fields above. No inferences, no snapshot diffs:

| Gate | Today's check | New check |
|---|---|---|
| Channel-edges | `predecessor.is_active_for_ordering()` (consults `host_states.values().all(...)` heuristic; needed `terminal_at` retrofit) | Predecessor channel's rollout `state == Converged` for all its hosts. Direct read. |
| Soak gate (Healthy → Soaked) | `host_probes_observed && host_probes_passing && soak_elapsed` — observed/passing inferred from current checkin snapshot | `probe_observed_first_at.is_some() && now > soak_due_at && all_enforce_mode_probes_pass` (per RFC-0010 §3.3) |
| Sustained-failure sweep | CP wallclock first-noticed `first_seen`, threshold = 60 s | Removed from CP. Agent reports `Failed` event directly when its own threshold elapses (agent has true probe-failure-start timestamp). |
| Quarantine (dispatch refuses bad SHA) | Channel's `quarantined_closures` table | Same. Populated by `RollbackComplete` handler. (No change from v0.2 fix #6.) |

## 7. Wire version

v0.2 is a fresh wire revision (`X-Nixfleet-Protocol: 2`). The pre-v0.2 contract (checkin-derived state, `/v1/agent/checkin`, `/v1/agent/report`, the `HostRolloutState` enum's 9 variants) is **not preserved** — CP rejects requests at `X-Nixfleet-Protocol: 1` outright. The deleted surface:

- `crates/nixfleet-control-plane/src/server/routes/checkin.rs` (state-deriving endpoint) → deleted; replaced by `routes/events.rs` + `routes/heartbeat.rs`.
- `crates/nixfleet-control-plane/src/server/reconcile.rs` `health_sweep` block (`HEALTH_FAILURE_THRESHOLD_SECS`, `first_seen` tracker) → deleted; agent owns the sweep timer.
- `host_dispatch_state` + `host_rollout_state` DB tables → replaced by a single `host_rollout_records` table mapping `(rollout_id, hostname) → HostRolloutRecord` (§5).
- `Healthy` / `Soaked` / `Queued` / `Dispatched` / `ConfirmWindow` variants in `HostRolloutState` → removed; 6 variants remain (§3).
- View-layer label conditionals introduced in `2d5b92ef` to mask the loose state machine → removed; one canonical label per state, no `current`/`declared` disambiguation needed.

Because v0.2 ships a new fleet image end-to-end (operator runs `fleet-up`, agents and CP roll out together as part of the same closure), there are no in-place upgrades and no mixed-version fleets to support. Pre-v0.2 deployments migrate by running v0.2's `fleet-up` against fresh hosts.

## 8. Operator-visible improvements

- **`nixfleet status --rollout-history <rollout-id>`** — natural with explicit timestamps per transition. Renders the event log directly.
- **No CLI label conditionals.** Pre-v0.2 had to map `Failed + current != declared = "→ reverting"` in the view layer to mask schema-permitted nonsense. Under this RFC, that state shape can't exist — `Failed` and `current != declared` always transitions to `Reverted`.
- **Bounded sweep latency.** Agent's `Failed` event arrives within one HTTP RTT of the threshold elapsing. No 60 s + checkin-lag tail.
- **No stale-probe gate satisfaction.** Probe cache reset on `ActivationComplete` means the soak gate cannot be misled by results from the prior closure.
- **Per-rollout timeline.** Every transition timestamp is preserved on the `HostRolloutRecord`. Operators can answer "when exactly did this rollout start soaking?" without trawling logs.

## 9. Open questions

1. **Sweep ownership.** §6 moves the sustained-failure threshold to the agent. Tradeoff: agent has exact timing but operators lose centralised configurability (the legacy `HEALTH_FAILURE_THRESHOLD_SECS = 60` was one CP constant; under §6 each agent reads it from `services.nixfleet-agent.healthChecks`). Acceptable; arguably better — threshold becomes per-fleet-config rather than per-CP-binary.
2. **Event delivery semantics.** "Retry with same `seq`, CP dedupes" is at-least-once. Should we promise exactly-once via an explicit `Ack` from CP on each event? Simpler today, room to add later.
3. **Inbound channel for `Dispatch`** (the only queued message). Today's pull-only contract (RFC-0003 §2.1) says agent initiates every connection. Three options:
   - **Short-poll** at agent's heartbeat interval — simplest, ~60 s latency on dispatch.
   - **Long-poll** at `GET /v1/agent/dispatch?wait=60` — sub-second latency, still pull-only.
   - **SSE / HTTP/2 server-push** — lowest latency, requires holding connections.
   Recommendation: **long-poll**, preserves the "agent always initiates" property of RFC-0003 §2.1 without the latency hit. Decide before implementation.
4. **Replay-from-seq protocol.** §4.3 sketches it; the full handshake (which seq is "last seen" per rollout, how the agent walks its event log, how it handles gaps in its own log after a restart) needs a worked example.
5. **Boot recovery.** The agent on boot emits a synthetic `ActivationComplete` reporting whatever `/run/current-system` is, so CP can re-sync with reality. Edge case: agent was mid-rollout when host rebooted — agent doesn't know its prior `rollout_id`. Resolution: agent's first heartbeat after boot includes `current_closure`; if it matches some rollout's `target_closure` in CP's view, CP synthesises a `Converged` for that rollout. Needs explicit handling spec.
6. **Compliance events.** ~~Pre-v0.2 `compliance-failure` reports come through `/v1/agent/report` independently. v0.2 should fold them into the unified `/v1/agent/events` stream — same idempotency story, same `seq` ordering. Add a `ComplianceFailure` event variant to §4.2; out of scope for this RFC's first cut but should land in the same v0.2 cycle.~~ **RESOLVED in RFC-0010 §7.** Compliance is folded as a probe **kind** (`kind = "evidence"`), not as a separate event variant. The existing `ProbeResult` event carries per-control granularity via the v0.2.1 `sub_results` field (§4.2 above + RFC-0010 §7.1). No `ComplianceFailure` event type exists; compliance gate-input flows through the unified probe pipeline.
7. **Event log persistence on the agent side.** If the agent crashes between firing the switch and reporting `ActivationComplete`, the event is lost. Options: persist outbound events to disk before sending (durable queue), or rely on the heartbeat drift-detection to recover. Pick one before the §10 property tests can be defined.

## 10. Validation plan

- **State-machine property tests.** For every event sequence, assert the post-event state satisfies the §3 invariants (`Converged ⇒ current == declared`, etc.). Property-based via `proptest`. Tests live in `crates/nixfleet-control-plane/src/db/host_rollout_records.rs`.
- **Event-loss tolerance.** Drop arbitrary events from a known-good sequence; assert that the next heartbeat's drift detection triggers `Replay-From` and final state matches the no-loss run.
- **Agent-restart recovery.** Kill the agent mid-activation; assert that the post-restart heartbeat + drift-handshake brings CP's record back into agreement with `/run/current-system`.
- **CP-offline rollback.** Kill CP between the agent's `Failed` event POST and `RollbackComplete` event POST. Assert that the agent still completes the rollback (manifest-driven, no CP signal needed), then replays both events to CP once it comes back up.
- **Demo re-validation.** Re-run the v0.2 demo's full cycle (`fleet-up` → `fleet-promote` → `fleet-rollback` → `fleet-recover` → `fleet-down`) and confirm:
  - Zero `current != declared` snapshots in `Soaking` / `Converged` / `Reverted` states.
  - Sweep fires within `HEALTH_FAILURE_THRESHOLD ± 5 s` of probes-first-failed-at (was: + checkin-lag, observed at ~89 s for a 60 s threshold).
  - `current_closure_hash` updates within one HTTP RTT of `RollbackComplete` (was: ~60 s lag waiting for next checkin).
  - CLI never renders a `→ reverting` state (state model forbids the underlying shape).
  - CP receives **no** outbound message destined for an agent during the rollback cycle (pull-only verified; `RollbackSignal` is gone).
