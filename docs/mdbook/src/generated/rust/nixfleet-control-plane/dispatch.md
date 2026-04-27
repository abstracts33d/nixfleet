# `nixfleet_control_plane::dispatch`

Phase 4 dispatch loop — bridge from `fleet.resolved.json`
(CI signed) to `CheckinResponse.target` (agent activates).

Per ARCHITECTURE.md the CP holds no opinions: it routes hosts to
their declared target as evaluated by CI. The decision per
checkin is a 3-way comparison:

1. The host's current generation (from `CheckinRequest`).
2. The host's declared target (`fleet.resolved.hosts[h].closureHash`).
3. Whether a `pending_confirms` row is already in flight.

The reconciler crate (`nixfleet-reconciler`) emits a richer
`Action` stream — waves, soaking, halts — for log/observability
and future Phase 5+ wave staging. For Phase 4 minimum the per-host
dispatch is a direct comparison; no reconciler state machine is
required to close the activation chain. When wave staging lands
the wave/soak gates plug in *before* this decision.

The function in this module is pure: no I/O, clock injected. The
caller (the `/v1/agent/checkin` handler in `server.rs`) is
responsible for the DB lookup + insert side effects.

## Items

### 🔓 `enum Decision`

Outcome of the dispatch decision for a host.

`PartialEq` is intentionally NOT derived: `EvaluatedTarget`
doesn't implement it, and the equality semantics on a freshly-
allocated `evaluated_at` are not meaningful anyway. Tests pattern-
match the variants directly.


### 🔓 `fn decide_target`

Pure dispatch decision.

`pending_for_host` is `true` if the DB has any `pending_confirms`
row in state `'pending'` for this hostname (regardless of which
rollout). The caller queries the DB and passes the bool — keeps
this function pure and trivially unit-testable.


