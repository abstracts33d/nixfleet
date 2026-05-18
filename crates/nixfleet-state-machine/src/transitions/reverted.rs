//! Transitions from `Reverted`. Terminal until the channel halt is lifted
//! by a new declared SHA (RFC-0005 §3). The runtime creates a fresh
//! `HostRolloutState` when the next rollout opens for this channel; this
//! record stays `Reverted` as history.
//!
//! All events return `IllegalForState`. Like `Converged`, this branch
//! exists to catch late-arriving events that shouldn't undo the
//! terminal state.

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutPolicy;

use crate::effect::Effect;
use crate::error::TransitionError;
use crate::event::Event;
use crate::state::HostRolloutState;

use super::illegal;

pub(super) fn handle(
    state: HostRolloutState,
    event: Event,
    _now: DateTime<Utc>,
    _policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    Err(illegal(&state, &event))
}
