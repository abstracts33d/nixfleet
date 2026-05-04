//! Top-level `reconcile` orchestration.

use crate::observed::DeferralRecord;
use crate::rollout_state::{self, RolloutState};
use crate::{Action, Observed};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;

/// Predecessor channel that has not converged its most-recent rollout, if any.
/// Channels with no rollout history are treated as open (proceed) — edges
/// constrain ordering between *active* rollouts, not "must have at least one
/// rollout ever". `Observed.active_rollouts` carries every non-terminal
/// rollout; absence is the gate.
fn predecessor_channel_blocking(
    fleet: &FleetResolved,
    observed: &Observed,
    channel: &str,
) -> Option<String> {
    fleet
        .channel_edges
        .iter()
        .filter(|e| e.after == channel)
        .find_map(|e| {
            let predecessor_active = observed
                .active_rollouts
                .iter()
                .any(|r| r.channel == e.before);
            if predecessor_active {
                Some(e.before.clone())
            } else {
                None
            }
        })
}

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
        if has_active || !fleet.channels.contains_key(channel) {
            continue;
        }
        if let Some(blocker) = predecessor_channel_blocking(fleet, observed, channel) {
            // Debounce: only emit when (target_ref, blocked_by) would change.
            let proposed = DeferralRecord {
                target_ref: current_ref.clone(),
                blocked_by: blocker.clone(),
            };
            if observed.last_deferrals.get(channel) != Some(&proposed) {
                actions.push(Action::RolloutDeferred {
                    channel: channel.clone(),
                    target_ref: current_ref.clone(),
                    blocked_by: blocker.clone(),
                    reason: format!(
                        "channel '{blocker}' has an active rollout — channelEdges holds OpenRollout until predecessor converges",
                    ),
                });
            }
            continue;
        }
        actions.push(Action::OpenRollout {
            channel: channel.clone(),
            target_ref: current_ref.clone(),
        });
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

#[cfg(test)]
mod channel_edge_tests {
    use super::*;
    use crate::host_state::HostRolloutState;
    use crate::observed::{HostState, Rollout};
    use nixfleet_proto::{
        Channel, ChannelEdge, Compliance, FleetResolved, Host, Meta, OnHealthFailure, RolloutPolicy,
    };
    use std::collections::HashMap;

    fn fleet_with_channel_edges(edges: Vec<ChannelEdge>) -> FleetResolved {
        let mut channels = HashMap::new();
        for ch in ["db", "app"] {
            channels.insert(
                ch.to_string(),
                Channel {
                    rollout_policy: "p".into(),
                    reconcile_interval_minutes: 30,
                    freshness_window: 1440,
                    signing_interval_minutes: 60,
                    compliance: Compliance {
                        frameworks: vec![],
                        mode: "disabled".into(),
                    },
                },
            );
        }
        let mut rollout_policies = HashMap::new();
        rollout_policies.insert(
            "p".into(),
            RolloutPolicy {
                strategy: "all-at-once".into(),
                waves: vec![],
                health_gate: Default::default(),
                on_health_failure: OnHealthFailure::Halt,
            },
        );
        FleetResolved {
            schema_version: 1,
            hosts: HashMap::new(),
            channels,
            rollout_policies,
            waves: HashMap::new(),
            edges: vec![],
            channel_edges: edges,
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: "ed25519".into(),
            },
        }
    }

    fn observed_with_active_rollout_on(channel: &str) -> Observed {
        let mut o = Observed::default();
        o.channel_refs.insert("app".into(), "ref-app-1".into());
        o.active_rollouts.push(Rollout {
            id: format!("{channel}-rollout"),
            channel: channel.into(),
            target_ref: format!("ref-{channel}-active"),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states: HashMap::new(),
            last_healthy_since: HashMap::new(),
        });
        o
    }

    #[test]
    fn channel_edge_with_active_predecessor_defers_rather_than_opens() {
        let fleet = fleet_with_channel_edges(vec![ChannelEdge {
            before: "db".into(),
            after: "app".into(),
            reason: Some("schema-migration".into()),
        }]);
        let observed = observed_with_active_rollout_on("db");
        let now = chrono::Utc::now();
        let actions = reconcile(&fleet, &observed, now);

        // Must NOT contain OpenRollout for app.
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::OpenRollout { channel, .. } if channel == "app")),
            "app rollout should be held while db has an active rollout: {actions:?}"
        );
        // Must contain RolloutDeferred for app.
        let deferred = actions.iter().find_map(|a| match a {
            Action::RolloutDeferred {
                channel,
                blocked_by,
                ..
            } if channel == "app" => Some(blocked_by.clone()),
            _ => None,
        });
        assert_eq!(
            deferred.as_deref(),
            Some("db"),
            "expected RolloutDeferred(app, blocked_by=db); got {actions:?}",
        );
    }

    #[test]
    fn channel_edge_with_no_predecessor_history_proceeds() {
        // db has never had a rollout — RFC §4.3 punt resolves "predecessor
        // never released" as "proceed" (edges constrain ordering between
        // active rollouts, not a presence requirement).
        let fleet = fleet_with_channel_edges(vec![ChannelEdge {
            before: "db".into(),
            after: "app".into(),
            reason: None,
        }]);
        let mut observed = Observed::default();
        observed.channel_refs.insert("app".into(), "ref-app-1".into());
        let actions = reconcile(&fleet, &observed, chrono::Utc::now());

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::OpenRollout { channel, .. } if channel == "app")),
            "app should open with no db history: {actions:?}"
        );
    }

    #[test]
    fn rollout_deferred_is_debounced_via_last_deferrals() {
        let fleet = fleet_with_channel_edges(vec![ChannelEdge {
            before: "db".into(),
            after: "app".into(),
            reason: None,
        }]);
        let mut observed = observed_with_active_rollout_on("db");
        // Stamp the same deferral as already-emitted.
        observed.last_deferrals.insert(
            "app".into(),
            DeferralRecord {
                target_ref: "ref-app-1".into(),
                blocked_by: "db".into(),
            },
        );
        let actions = reconcile(&fleet, &observed, chrono::Utc::now());

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::RolloutDeferred { .. })),
            "RolloutDeferred must NOT re-fire when last_deferrals already records the same (target_ref, blocked_by): {actions:?}",
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::OpenRollout { channel, .. } if channel == "app")),
            "still blocked, must not open: {actions:?}",
        );
    }

    #[test]
    fn rollout_deferred_re_fires_on_blocker_change() {
        let fleet = fleet_with_channel_edges(vec![
            ChannelEdge {
                before: "db".into(),
                after: "app".into(),
                reason: None,
            },
            ChannelEdge {
                before: "infra".into(),
                after: "app".into(),
                reason: None,
            },
        ]);
        let mut observed = Observed::default();
        observed.channel_refs.insert("app".into(), "ref-app-1".into());
        // Active rollout on infra (not db) — different blocker than the
        // last-emitted record below.
        observed.active_rollouts.push(Rollout {
            id: "infra-rollout".into(),
            channel: "infra".into(),
            target_ref: "ref-infra".into(),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states: HashMap::new(),
            last_healthy_since: HashMap::new(),
        });
        // Need a third channel in fleet for completeness.
        observed.last_deferrals.insert(
            "app".into(),
            DeferralRecord {
                target_ref: "ref-app-1".into(),
                blocked_by: "db".into(),
            },
        );
        // Add infra to channels so the test fleet is consistent.
        let mut fleet = fleet;
        fleet.channels.insert(
            "infra".into(),
            Channel {
                rollout_policy: "p".into(),
                reconcile_interval_minutes: 30,
                freshness_window: 1440,
                signing_interval_minutes: 60,
                compliance: Compliance {
                    frameworks: vec![],
                    mode: "disabled".into(),
                },
            },
        );
        let actions = reconcile(&fleet, &observed, chrono::Utc::now());

        let deferred_blocker = actions.iter().find_map(|a| match a {
            Action::RolloutDeferred { blocked_by, .. } => Some(blocked_by.clone()),
            _ => None,
        });
        assert_eq!(
            deferred_blocker.as_deref(),
            Some("infra"),
            "blocker changed from db→infra; must re-emit: {actions:?}",
        );
    }

    #[test]
    fn channel_edge_clears_when_predecessor_converges() {
        // No active rollout on db means nothing to wait for.
        let fleet = fleet_with_channel_edges(vec![ChannelEdge {
            before: "db".into(),
            after: "app".into(),
            reason: None,
        }]);
        let mut observed = Observed::default();
        observed.channel_refs.insert("app".into(), "ref-app-1".into());
        // Even with a stale last_deferral entry, an unblocked channel opens.
        observed.last_deferrals.insert(
            "app".into(),
            DeferralRecord {
                target_ref: "ref-app-1".into(),
                blocked_by: "db".into(),
            },
        );
        let _ = HostState {
            online: true,
            current_generation: None,
        };
        let _ = HostRolloutState::Queued; // import-touch to keep the use clean
        let actions = reconcile(&fleet, &observed, chrono::Utc::now());

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::OpenRollout { channel, .. } if channel == "app")),
            "predecessor no longer active → must open: {actions:?}",
        );
    }

    fn _host(channel: &str, tags: &[&str]) -> Host {
        Host {
            system: "x86_64-linux".into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            channel: channel.into(),
            closure_hash: None,
            pubkey: None,
        }
    }
}
