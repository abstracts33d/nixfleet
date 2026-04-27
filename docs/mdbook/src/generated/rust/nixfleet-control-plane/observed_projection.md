# `nixfleet_control_plane::observed_projection`

Live `Observed` projection from in-memory checkin state.

Replaces Phase 2's hand-written `observed.json` as the default
source of truth for the reconcile loop. The file-backed input
stays as `--observed` for offline-replay debugging (operator
dumps in-memory state, reproduces a tick) and as a dev/test
fallback when no agents are checking in yet.

For now this is intentionally a dumb projection — every host
that has ever checked in shows up as `online`, with its most
recent `currentGeneration.closureHash` as the
`current_generation` field. Phase 4 introduces staleness
detection (host with no checkin in N intervals → online: false)
and active-rollout tracking; this module's signature stays the
same so PR-4's logic plugs in cleanly.

## Items

### 🔓 `fn project`

Build an `Observed` from the in-memory checkin records and the
channel-refs cache. Pure function — caller takes the read locks.


