//! Rollout reducer error path. Parallel to the per-host `TransitionError`
//! (`crate::error`). The applier logs + drops or surfaces depending on
//! the variant.

use thiserror::Error;

use crate::rollout::state::{RolloutId, RolloutState};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum RolloutTransitionError {
    /// The event is not legal from the current `RolloutState`. Typically
    /// out-of-order arrival (e.g., `RolloutTerminal` from `Opening` before
    /// any host joined).
    #[error("event {event} not legal from rollout state {from:?} (rollout {rollout_id})")]
    IllegalForState {
        from: RolloutState,
        event: &'static str,
        rollout_id: RolloutId,
    },

    /// A `RolloutOpened` arrived for a `(channel, ref)` whose channel
    /// already has an `active_rollout_id`. The planner must emit
    /// `SuccessorOpened` first (RFC-0012 §3 invariant).
    #[error(
        "rollout {rollout_id} opened on channel {channel} without prior SuccessorOpened (expected supersession)"
    )]
    SupersessionExpected {
        rollout_id: RolloutId,
        channel: String,
    },

    /// Invariant from RFC-0012 §3 violated by the event's payload (e.g.,
    /// `RolloutTerminal` while at least one host is not Converged).
    #[error("rollout invariant violation: {0}")]
    Invariant(&'static str),

    /// Reducer arm not yet implemented. Returned by the Phase 10a
    /// skeleton; every variant gets a real arm in Phase 10b. Should never
    /// be reached after Phase 10 closes — if it is, that's a code defect.
    #[error("rollout transition not yet implemented for event {event} from state {from:?}")]
    Unimplemented {
        from: RolloutState,
        event: &'static str,
    },
}
