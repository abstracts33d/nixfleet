# `nixfleet_control_plane::server::reconcile`

Background reconcile loop.

Runs every [`RECONCILE_INTERVAL`] (30s default), reads the
in-memory projection of host checkins + Forgejo channel-refs,
verifies the build-time `--artifact` against the trust file,
reconciles, and writes the resulting `FleetResolved` snapshot
into `AppState.verified_fleet` — *only* when the new bytes are
at least as fresh as what's already there. The Forgejo poll
task is the other writer; the freshness gate keeps its
Forgejo-fresh snapshot from being clobbered by the static
build-time bytes.

## Items

### 🔐 `fn spawn_reconcile_loop`

Spawn the reconcile loop. Each tick:
1. Reads the channel-refs cache (refreshed by the Forgejo poll
   task; falls back to file-backed observed.json when empty).
2. Builds an `Observed` from the in-memory checkin state +
   cached channel-refs.
3. Verifies the resolved artifact and reconciles against the
   projected `Observed`.
4. Emits the plan via tracing.

Errors at any step are logged and fall through; the loop never
crashes on transient failures.


### 🔒 `fn run_tick_with_projection`

Run a tick using the in-memory projection rather than reading
`observed.json`. Mirrors `crate::tick` but takes the projected
`Observed` from the caller.

Returns both the tick output (for the journal plan) and the
verified `FleetResolved` (for the dispatch path's snapshot in
`AppState`). The fleet is `None` when the tick failed verify —
the caller preserves whatever snapshot was previously in place.


### 🔐 `fn verify_fleet_only`

Verify-only variant for the empty-projection fallback path. The
caller runs the rest of the tick via `crate::tick` — this just
produces the verified fleet snapshot for `AppState.verified_fleet`.
Returns `None` when verify fails; the caller preserves the prior
snapshot.


