//! Transitions from `Converged`. Terminal for this rollout — the runtime
//! creates a NEW `HostRolloutState` (a fresh record with a new `rollout_id`)
//! when the channel opens its next rollout. The existing `Converged` record
//! is read-only history from this point.
//!
//! All events return `IllegalForState`. The runtime layer logs + drops
//! (late retransmits are idempotent at the wire layer via `(host, rollout,
//! seq)` dedup; this branch is for events that arrive AFTER the record
//! has reached Converged and shouldn't move it elsewhere).

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
