//! Top-level `reconcile`: RFC-0002 §4 steps 1–6 orchestration.

use crate::{rollout_state, Action, Observed};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;

pub fn reconcile(fleet: &FleetResolved, observed: &Observed, _now: DateTime<Utc>) -> Vec<Action> {
    let mut actions = Vec::new();

    // §4 step 2: open rollouts for channels whose ref changed.
    for (channel, current_ref) in &observed.channel_refs {
        if observed.last_rolled_refs.get(channel) == Some(current_ref) {
            continue;
        }
        let has_active = observed
            .active_rollouts
            .iter()
            .any(|r| &r.channel == channel && (r.state == "Executing" || r.state == "Planning"));
        if !has_active && fleet.channels.contains_key(channel) {
            actions.push(Action::OpenRollout {
                channel: channel.clone(),
                target_ref: current_ref.clone(),
            });
        }
    }

    // §4 step 4: advance each Executing rollout.
    for rollout in &observed.active_rollouts {
        actions.extend(rollout_state::advance_rollout(fleet, observed, rollout));
    }

    actions
}
