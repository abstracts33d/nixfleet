//! Rollout-reducer transition dispatch (RFC-0012 §3 + §7).
//!
//! Each source-state module holds the (legal-event) match arms; this
//! file is the single entry point the public `step()` calls. The
//! reducer enforces transition legality — the applier owns aggregation
//! (e.g., "all hosts Converged → emit `RolloutTerminal`") so the
//! reducer can stay pure.

use chrono::{DateTime, Utc};

use crate::rollout::effect::RolloutEffect;
use crate::rollout::error::RolloutTransitionError;
use crate::rollout::event::RolloutEvent;
use crate::rollout::state::{RolloutRecord, RolloutState};

mod active;
mod converging;
mod failed;
mod opening;
mod pruned;
mod reverted;
mod superseded;
mod terminal;

/// Dispatch a `RolloutEvent` against the current `RolloutRecord`.
pub(crate) fn dispatch(
    record: RolloutRecord,
    event: RolloutEvent,
    now: DateTime<Utc>,
) -> Result<(RolloutRecord, Vec<RolloutEffect>), RolloutTransitionError> {
    match record.state {
        RolloutState::Opening => opening::step(record, event, now),
        RolloutState::Active => active::step(record, event, now),
        RolloutState::Converging => converging::step(record, event, now),
        RolloutState::Terminal => terminal::step(record, event, now),
        RolloutState::Reverted => reverted::step(record, event, now),
        RolloutState::Failed => failed::step(record, event, now),
        RolloutState::Superseded => superseded::step(record, event, now),
        RolloutState::Pruned => pruned::step(record, event, now),
    }
}

/// Helper: build the [`RolloutEffect::RecordRolloutTransition`] effect
/// for a state transition. Lives here so the per-state modules don't
/// re-derive it.
pub(super) fn transition_effect(
    record: &RolloutRecord,
    from: RolloutState,
    to: RolloutState,
    at: DateTime<Utc>,
) -> RolloutEffect {
    RolloutEffect::RecordRolloutTransition {
        rollout_id: record.rollout_id.clone(),
        from,
        to,
        at,
    }
}

/// Helper: build a `RolloutTransitionError::IllegalForState` for events
/// that don't apply to the current state.
pub(super) fn illegal(
    from: RolloutState,
    event: &RolloutEvent,
    rollout_id: crate::rollout::state::RolloutId,
) -> RolloutTransitionError {
    RolloutTransitionError::IllegalForState {
        from,
        event: event.kind(),
        rollout_id,
    }
}
