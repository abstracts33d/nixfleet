//! Top-level `reconcile`: RFC-0002 §4 steps 1–6 orchestration.

use crate::{rollout_state, Action, Observed};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;

pub fn reconcile(fleet: &FleetResolved, observed: &Observed, now: DateTime<Utc>) -> Vec<Action> {
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

    // §4 step 4: advance each Executing rollout. `now` flows down
    // to the per-host arm so the soak-timer gate (RFC-0002 §3.2
    // Healthy → Soaked) can compare against last_healthy_since.
    //
    // Issue #21: a rollout referencing a channel that no longer
    // exists in fleet.resolved.channels gets a ChannelUnknown
    // observability event before the silent-continue in
    // advance_rollout fires. Operators can grep the journal for
    // these to spot teardown drift.
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
