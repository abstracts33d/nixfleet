//! Top-level `reconcile` orchestration.

use crate::rollout_state::{self, RolloutState};
use crate::{Action, Observed};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;

pub fn reconcile(fleet: &FleetResolved, observed: &Observed, now: DateTime<Utc>) -> Vec<Action> {
    let mut actions = Vec::new();

    // Open rollouts for channels whose ref changed.
    for (channel, current_ref) in &observed.channel_refs {
        if observed.last_rolled_refs.get(channel) == Some(current_ref) {
            continue;
        }
        let has_active = observed.active_rollouts.iter().any(|r| {
            &r.channel == channel
                && matches!(r.state, RolloutState::Executing | RolloutState::Planning)
        });
        if !has_active && fleet.channels.contains_key(channel) {
            actions.push(Action::OpenRollout {
                channel: channel.clone(),
                target_ref: current_ref.clone(),
            });
        }
    }

    // Advance each Executing rollout. Channel-removed rollouts emit a
    // ChannelUnknown observability event before silent-continue.
    for rollout in &observed.active_rollouts {
        if !fleet.channels.contains_key(&rollout.channel) {
            actions.push(Action::ChannelUnknown {
                channel: rollout.channel.clone(),
            });
            continue;
        }
        actions.extend(rollout_state::advance_rollout(fleet, observed, rollout, now));
    }

    actions
}
