//! Snapshot rehydration — effect emission rules for state restored from a
//! CP-supplied `HostRolloutSnapshot` (RFC-0008 §9.5 / LIFT #3).
//!
//! LIFT #4: bootstrap-applied state must re-prime worker channels via the
//! same `Effect` contract as ordinary transitions. The agent's reducer
//! caches `HostRolloutState` directly from the snapshot (the canonical
//! state lives on CP, not in an event ladder), but every worker that
//! re-primes on a state change — probe runners, activation drainer,
//! soak detector — must learn about the rehydration through an Effect.
//!
//! Adding a new worker re-priming need lands here (one match arm), not
//! in every bootstrap entry point.

use crate::effect::Effect;
use crate::state::{HostRolloutState, HostState};

/// Effects to emit immediately after applying a `HostRolloutSnapshot` to
/// the agent's in-memory state. Workers consume these to refresh any
/// cached per-rollout state seeded by prior process incarnations.
///
/// `Pending` emits nothing: no rollout is in flight, no probes were ever
/// declared for it locally, no worker has any cached state to invalidate.
///
/// Every other state implies the rollout is (or was) live for this host —
/// probe runners may be holding tickers tagged with a stale `rollout_id`
/// from a prior agent process, so `LocalResetProbeCache` fires
/// unconditionally to force the probe worker to drop those tickers and
/// reload declarations from `health-checks.json` under the bootstrapped
/// `rollout_id`.
pub fn rehydration_effects(state: &HostRolloutState) -> Vec<Effect> {
    match state.state {
        HostState::Pending => Vec::new(),
        HostState::Activating
        | HostState::Deferred
        | HostState::Soaking
        | HostState::Converged
        | HostState::Failed
        | HostState::Reverted => vec![Effect::LocalResetProbeCache {
            rollout_id: state.rollout_id.clone(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::state::HostState;

    fn state_in(s: HostState) -> HostRolloutState {
        let mut st = HostRolloutState::new_pending(
            "stable@r1".into(),
            "h1".into(),
            "stable".into(),
            "target".into(),
            Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 18, 0, 5, 0).unwrap(),
        );
        st.state = s;
        st
    }

    #[test]
    fn pending_emits_nothing() {
        assert!(rehydration_effects(&state_in(HostState::Pending)).is_empty());
    }

    #[test]
    fn non_pending_states_emit_probe_cache_reset() {
        for s in [
            HostState::Activating,
            HostState::Deferred,
            HostState::Soaking,
            HostState::Converged,
            HostState::Failed,
            HostState::Reverted,
        ] {
            let effects = rehydration_effects(&state_in(s));
            assert!(
                effects.iter().any(|e| matches!(
                    e,
                    Effect::LocalResetProbeCache { rollout_id } if rollout_id.as_str() == "stable@r1"
                )),
                "state {s:?} must emit LocalResetProbeCache; got {effects:?}",
            );
        }
    }
}
