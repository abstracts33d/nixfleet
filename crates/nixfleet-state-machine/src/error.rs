//! Reducer error path. Distinct from runtime errors — these mean the input
//! event is structurally inapplicable to the current state, which is a
//! runtime invariant violation (out-of-order event, stale seq, etc.). The
//! runtime layer decides whether to log + drop, request replay, or panic.

use thiserror::Error;

use crate::state::{HostState, RolloutId};

#[derive(Debug, Error, Clone, PartialEq)]
pub enum TransitionError {
    /// The event is not legal from the current `HostState`. The runtime
    /// layer should typically log + drop (lost-ordering noise) and rely on
    /// heartbeat drift-detection to recover via Replay-From (RFC-0005 §4.3).
    #[error("event {event} not legal from state {from:?} (rollout {rollout_id}, host {hostname})")]
    IllegalForState {
        from: HostState,
        event: &'static str,
        rollout_id: RolloutId,
        hostname: String,
    },

    /// Event `seq` is not strictly greater than `last_event_seq`. Could be
    /// a retransmit (idempotent, runtime dedupes on `(host, rollout, seq)`)
    /// or out-of-order arrival.
    #[error(
        "event seq {got} is not > last_event_seq {last} (rollout {rollout_id}, host {hostname})"
    )]
    SeqRegression {
        got: u64,
        last: u64,
        rollout_id: RolloutId,
        hostname: String,
    },

    /// Invariant from RFC-0005 §3 violated by the event's payload (e.g.
    /// `Converged` claimed but `current != target`). CP rejects the event
    /// with `409 Conflict`; agent retries after re-verifying.
    #[error("invariant violation: {0}")]
    Invariant(&'static str),

    /// Reducer arm not yet implemented. Returned by the Phase 3a skeleton;
    /// every variant gets a real arm in Phase 3b. Should never be reached
    /// after Phase 3 closes — if it is, that's a code defect.
    #[error("transition not yet implemented for event {event} from state {from:?}")]
    Unimplemented {
        from: HostState,
        event: &'static str,
    },
}
