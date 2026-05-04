//! Per-host state machine. Emits actions and tracks wave soaked-ness.

use crate::observed::{Observed, Rollout};
use crate::Action;
use chrono::{DateTime, Utc};
use nixfleet_proto::{FleetResolved, Wave};

/// Disruption-budget evaluation for the dispatch gate.
///
/// Architectural note: budgets are read from the rollout's snapshot
/// (frozen at OpenRollout time, signed into the rollout manifest), NOT
/// re-resolved against live `fleet.hosts.tags` per tick. Mid-rollout
/// retags therefore cannot reshape budget membership — the cascading-
/// dispatch hazard from live resolution is structurally impossible.
///
/// Cross-rollout fleet-wide enforcement survives the snapshot model:
/// each rollout carries its own snapshot, but in-flight summing matches
/// budgets across rollouts by `selector` equality so the "max one
/// workstation in flight, ever" semantics hold even with multiple
/// rollouts active simultaneously.
mod budgets {
    use super::HostRolloutState;
    use crate::observed::{Observed, Rollout};
    use nixfleet_proto::{RolloutBudget, Selector};

    /// Sum of in-flight hosts across all active rollouts whose snapshot
    /// has a budget with the matching `selector`. Match by selector
    /// equality (not list index) so reordering `fleet.disruptionBudgets`
    /// between rollout opens doesn't conflate distinct budgets.
    pub(super) fn in_flight_count(observed: &Observed, selector: &Selector) -> u32 {
        observed
            .active_rollouts
            .iter()
            .map(|r| {
                let Some(b) = r.budgets.iter().find(|rb| &rb.selector == selector) else {
                    return 0;
                };
                r.host_states
                    .iter()
                    .filter(|(h, st)| {
                        if !b.hosts.iter().any(|bh| bh == *h) {
                            return false;
                        }
                        matches!(
                            st,
                            HostRolloutState::Dispatched
                                | HostRolloutState::Activating
                                | HostRolloutState::ConfirmWindow
                                | HostRolloutState::Healthy
                        )
                    })
                    .count() as u32
            })
            .sum()
    }

    /// Tightest (in_flight, max_in_flight) across `rollout`'s snapshot
    /// budgets that include `host`. Returns `None` when no budget
    /// gates `host` (no `max_in_flight`, or host not in any budget's
    /// frozen membership).
    pub(super) fn budget_max(
        observed: &Observed,
        rollout: &Rollout,
        host: &str,
    ) -> Option<(u32, u32)> {
        rollout
            .budgets
            .iter()
            .filter(|b| b.hosts.iter().any(|h| h == host))
            .filter_map(|b: &RolloutBudget| {
                b.max_in_flight
                    .map(|max| (in_flight_count(observed, &b.selector), max))
            })
            .min_by_key(|(_, max)| *max)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::observed::Rollout;
        use crate::rollout_state::RolloutState;
        use nixfleet_proto::{RolloutBudget, Selector};
        use std::collections::HashMap;

        fn family_selector() -> Selector {
            Selector {
                tags: vec!["family".into()],
                ..Default::default()
            }
        }

        fn rollout_with(
            id: &str,
            channel: &str,
            host_states: Vec<(&str, HostRolloutState)>,
            budgets: Vec<RolloutBudget>,
        ) -> Rollout {
            Rollout {
                id: id.into(),
                channel: channel.into(),
                target_ref: "ref".into(),
                state: RolloutState::Executing,
                current_wave: 0,
                host_states: host_states
                    .into_iter()
                    .map(|(h, s)| (h.to_string(), s))
                    .collect(),
                last_healthy_since: HashMap::new(),
                budgets,
            }
        }

        fn observed_with(rollouts: Vec<Rollout>) -> Observed {
            Observed {
                channel_refs: HashMap::new(),
                last_rolled_refs: HashMap::new(),
                host_state: HashMap::new(),
                active_rollouts: rollouts,
                compliance_failures_by_rollout: HashMap::new(),
                last_deferrals: HashMap::new(),
            }
        }

        #[test]
        fn budget_max_reads_from_rollout_snapshot() {
            // Snapshot says {a, b, c}; live tags don't matter because the
            // budget gate consults the rollout's frozen membership.
            let snap = RolloutBudget {
                selector: family_selector(),
                hosts: vec!["a".into(), "b".into(), "c".into()],
                max_in_flight: Some(2),
                max_in_flight_pct: None,
            };
            let r = rollout_with(
                "r",
                "stable",
                vec![
                    ("a", HostRolloutState::Dispatched),
                    ("b", HostRolloutState::Activating),
                    ("c", HostRolloutState::Queued),
                ],
                vec![snap],
            );
            let observed = observed_with(vec![r.clone()]);
            let (in_flight, max) = budget_max(&observed, &r, "c").expect("c in budget");
            assert_eq!((in_flight, max), (2, 2));
        }

        #[test]
        fn budget_max_returns_none_when_host_outside_snapshot() {
            let snap = RolloutBudget {
                selector: family_selector(),
                hosts: vec!["a".into(), "b".into()],
                max_in_flight: Some(1),
                max_in_flight_pct: None,
            };
            let r = rollout_with(
                "r",
                "stable",
                vec![("a", HostRolloutState::Dispatched)],
                vec![snap],
            );
            let observed = observed_with(vec![r.clone()]);
            assert!(budget_max(&observed, &r, "z").is_none());
        }

        #[test]
        fn cross_rollout_in_flight_sums_by_selector_identity() {
            // Two active rollouts with budgets sharing the same selector.
            // Cross-rollout fleet-wide enforcement: lab dispatched in r1
            // counts towards the shared budget when checking krach in r2.
            let snap_r1 = RolloutBudget {
                selector: family_selector(),
                hosts: vec!["lab".into(), "krach".into()],
                max_in_flight: Some(1),
                max_in_flight_pct: None,
            };
            let snap_r2 = RolloutBudget {
                selector: family_selector(),
                hosts: vec!["krach".into(), "ohm".into()],
                max_in_flight: Some(1),
                max_in_flight_pct: None,
            };
            let r1 = rollout_with(
                "r1",
                "edge",
                vec![("lab", HostRolloutState::Dispatched)],
                vec![snap_r1],
            );
            let r2 = rollout_with(
                "r2",
                "stable",
                vec![("krach", HostRolloutState::Queued)],
                vec![snap_r2],
            );
            let observed = observed_with(vec![r1, r2.clone()]);
            let (in_flight, max) =
                budget_max(&observed, &r2, "krach").expect("krach in r2 budget");
            assert_eq!(in_flight, 1, "lab in r1 contributes via selector identity");
            assert_eq!(max, 1);
        }

        #[test]
        fn snapshot_is_frozen_against_mid_rollout_retag() {
            // The architectural invariant: rollout topology is immutable
            // for the rollout's life. A retag mid-flight cannot reshape
            // the budget membership the rollout is enforcing.
            let snap = RolloutBudget {
                selector: family_selector(),
                hosts: vec!["a".into(), "b".into(), "c".into()],
                max_in_flight: Some(2),
                max_in_flight_pct: None,
            };
            let r = rollout_with(
                "r",
                "stable",
                vec![
                    ("a", HostRolloutState::Dispatched),
                    ("b", HostRolloutState::Dispatched),
                    ("c", HostRolloutState::Queued),
                ],
                vec![snap],
            );
            // The "retag" — operator changes b's tags in fleet.nix —
            // would normally drop b from the family selector. Snapshot
            // model means the rollout's view of family budget is
            // unchanged, so c sees the budget exhausted (a + b in
            // flight = 2 = max).
            let observed = observed_with(vec![r.clone()]);
            let (in_flight, max) = budget_max(&observed, &r, "c").unwrap();
            assert_eq!((in_flight, max), (2, 2));
        }
    }
}

/// Edge predecessor ordering check for the dispatch gate.
mod edges {
    use super::{lookup_host_state, HostRolloutState};
    use crate::observed::Rollout;
    use nixfleet_proto::FleetResolved;

    /// First incomplete (not Soaked/Converged) edge predecessor, if any.
    pub(super) fn predecessor_blocking(
        fleet: &FleetResolved,
        rollout: &Rollout,
        host: &str,
    ) -> Option<String> {
        fleet
            .edges
            .iter()
            .filter(|e| e.before == host)
            .find_map(|e| {
                let s = lookup_host_state(rollout, &e.after);
                if matches!(s, HostRolloutState::Soaked | HostRolloutState::Converged) {
                    None
                } else {
                    Some(e.after.clone())
                }
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::observed::Rollout;
        use nixfleet_proto::{Edge, FleetResolved, Meta};
        use std::collections::HashMap;

        fn fleet_with_edges(edges: Vec<Edge>) -> FleetResolved {
            FleetResolved {
                schema_version: 1,
                hosts: HashMap::new(),
                channels: HashMap::new(),
                rollout_policies: HashMap::new(),
                waves: HashMap::new(),
                edges,
                channel_edges: Vec::new(),
                disruption_budgets: Vec::new(),
                meta: Meta {
                    schema_version: 1,
                    signed_at: None,
                    ci_commit: None,
                    signature_algorithm: Some("ed25519".into()),
                },
            }
        }

        fn rollout_with_states(states: Vec<(&str, HostRolloutState)>) -> Rollout {
            use crate::rollout_state::RolloutState;
            let mut host_states = HashMap::new();
            for (h, s) in states {
                host_states.insert(h.to_string(), s);
            }
            Rollout {
                id: "r".into(),
                channel: "c".into(),
                target_ref: "ref".into(),
                state: RolloutState::Executing,
                current_wave: 0,
                host_states,
                last_healthy_since: HashMap::new(),
            budgets: vec![],
            }
        }

        #[test]
        fn no_edges_means_no_block() {
            let fleet = fleet_with_edges(vec![]);
            let rollout = rollout_with_states(vec![]);
            assert!(predecessor_blocking(&fleet, &rollout, "h1").is_none());
        }

        #[test]
        fn predecessor_done_is_not_blocking() {
            let fleet = fleet_with_edges(vec![Edge {
                before: "h1".into(),
                after: "h2".into(),
                reason: None,
            }]);
            let rollout = rollout_with_states(vec![("h2", HostRolloutState::Soaked)]);
            assert!(predecessor_blocking(&fleet, &rollout, "h1").is_none());
        }

        #[test]
        fn predecessor_queued_is_blocking() {
            let fleet = fleet_with_edges(vec![Edge {
                before: "h1".into(),
                after: "h2".into(),
                reason: None,
            }]);
            let rollout = rollout_with_states(vec![("h2", HostRolloutState::Queued)]);
            let blocker = predecessor_blocking(&fleet, &rollout, "h1");
            assert_eq!(blocker.as_deref(), Some("h2"));
        }
    }
}

pub use nixfleet_proto::HostRolloutState;

/// Defaults absent hosts to [`HostRolloutState::Queued`].
pub fn lookup_host_state(rollout: &Rollout, host: &str) -> HostRolloutState {
    rollout
        .host_states
        .get(host)
        .copied()
        .unwrap_or(HostRolloutState::Queued)
}

pub(crate) struct WaveOutcome {
    pub actions: Vec<Action>,
    pub wave_all_soaked: bool,
}

pub(crate) fn handle_wave(
    fleet: &FleetResolved,
    observed: &Observed,
    rollout: &Rollout,
    wave: &Wave,
    now: DateTime<Utc>,
) -> WaveOutcome {
    let mut out = WaveOutcome {
        actions: Vec::new(),
        wave_all_soaked: true,
    };

    for host in &wave.hosts {
        let state = lookup_host_state(rollout, host);
        match state {
            HostRolloutState::Queued => {
                out.wave_all_soaked = false;
                let online = observed
                    .host_state
                    .get(host)
                    .map(|h| h.online)
                    .unwrap_or(false);
                if !online {
                    out.actions.push(Action::Skip {
                        host: host.clone(),
                        reason: "offline".into(),
                    });
                    continue;
                }
                if let Some(predecessor) = edges::predecessor_blocking(fleet, rollout, host) {
                    out.actions.push(Action::Skip {
                        host: host.clone(),
                        reason: format!("edge predecessor {predecessor} incomplete"),
                    });
                    continue;
                }
                if let Some((in_flight, max)) = budgets::budget_max(observed, rollout, host) {
                    if in_flight >= max {
                        out.actions.push(Action::Skip {
                            host: host.clone(),
                            reason: format!("disruption budget ({in_flight}/{max} in flight)"),
                        });
                        continue;
                    }
                }
                out.actions.push(Action::DispatchHost {
                    rollout: rollout.id.clone(),
                    host: host.clone(),
                    target_ref: rollout.target_ref.clone(),
                });
            }
            HostRolloutState::Dispatched
            | HostRolloutState::Activating
            | HostRolloutState::ConfirmWindow => {
                out.wave_all_soaked = false;
            }
            HostRolloutState::Healthy => {
                // Healthy → Soaked once Healthy for `wave.soak_minutes`.
                // Without a `last_healthy_since` marker the soak gate
                // stays closed — better to wait than promote on missing data.
                out.wave_all_soaked = false;
                let soak_window = chrono::Duration::minutes(wave.soak_minutes as i64);
                if let Some(since) = rollout.last_healthy_since.get(host) {
                    if now.signed_duration_since(*since) >= soak_window {
                        out.actions.push(Action::SoakHost {
                            rollout: rollout.id.clone(),
                            host: host.clone(),
                        });
                    }
                }
            }
            HostRolloutState::Soaked | HostRolloutState::Converged => {}
            HostRolloutState::Failed | HostRolloutState::Reverted => {
                // `Failed` is reconciler-observed; `Reverted` is
                // agent-attested. Both halt; only `Failed` triggers
                // a fresh RollbackHost (Reverted is already rolled back).
                out.wave_all_soaked = false;
                if let Some(chan) = fleet.channels.get(&rollout.channel) {
                    if let Some(policy) = fleet.rollout_policies.get(&chan.rollout_policy) {
                        out.actions.push(Action::HaltRollout {
                            rollout: rollout.id.clone(),
                            reason: format!(
                                "host {host} {} (policy: {})",
                                state.as_db_str().to_lowercase(),
                                policy.on_health_failure
                            ),
                        });
                        if matches!(
                            policy.on_health_failure,
                            nixfleet_proto::OnHealthFailure::RollbackAndHalt
                        ) && matches!(state, HostRolloutState::Failed)
                        {
                            out.actions.push(Action::RollbackHost {
                                rollout: rollout.id.clone(),
                                host: host.clone(),
                                target_ref: rollout.target_ref.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_defaults_absent_to_queued() {
        use crate::rollout_state::RolloutState;
        let rollout = Rollout {
            id: "r".into(),
            channel: "c".into(),
            target_ref: "ref".into(),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states: std::collections::HashMap::new(),
            last_healthy_since: std::collections::HashMap::new(),
            budgets: vec![],
        };
        assert_eq!(
            lookup_host_state(&rollout, "missing"),
            HostRolloutState::Queued
        );
    }

    fn fleet_with_policy(on_health_failure: nixfleet_proto::OnHealthFailure) -> FleetResolved {
        use nixfleet_proto::{
            Channel, Compliance, Host, Meta, PolicyWave, RolloutPolicy, Selector,
        };
        use std::collections::HashMap;

        let mut hosts = HashMap::new();
        hosts.insert(
            "host-a".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: None,
                pubkey: None,
            },
        );
        let mut channels = HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "p".into(),
                reconcile_interval_minutes: 30,
                signing_interval_minutes: 60,
                freshness_window: 86400,
                compliance: Compliance {
                    mode: "permissive".into(),
                    frameworks: vec![],
                },
            },
        );
        let mut rollout_policies = HashMap::new();
        rollout_policies.insert(
            "p".to_string(),
            RolloutPolicy {
                strategy: "all-at-once".into(),
                waves: vec![PolicyWave {
                    selector: Selector {
                        tags: vec![],
                        tags_any: vec![],
                        hosts: vec![],
                        channel: None,
                        all: true,
                    },
                    soak_minutes: 0,
                }],
                health_gate: nixfleet_proto::HealthGate::default(),
                on_health_failure,
            },
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies,
            waves: HashMap::new(),
            edges: vec![],
            channel_edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: Some("ed25519".into()),
            },
        }
    }

    fn rollout_with_state(host: &str, state: HostRolloutState) -> Rollout {
        use crate::rollout_state::RolloutState;
        let mut host_states = std::collections::HashMap::new();
        host_states.insert(host.into(), state);
        Rollout {
            id: "stable@abc12345".into(),
            channel: "stable".into(),
            target_ref: "ref-xyz".into(),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states,
            last_healthy_since: std::collections::HashMap::new(),
            budgets: vec![],
        }
    }

    fn observed_online(host: &str) -> Observed {
        use crate::observed::HostState;
        let mut host_state = std::collections::HashMap::new();
        host_state.insert(
            host.into(),
            HostState {
                online: true,
                current_generation: None,
            },
        );
        Observed {
            channel_refs: std::collections::HashMap::new(),
            last_rolled_refs: std::collections::HashMap::new(),
            host_state,
            active_rollouts: vec![],
            compliance_failures_by_rollout: std::collections::HashMap::new(),
            last_deferrals: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn failed_under_halt_emits_only_halt_rollout() {
        let fleet = fleet_with_policy(nixfleet_proto::OnHealthFailure::Halt);
        let rollout = rollout_with_state("host-a", HostRolloutState::Failed);
        let observed = observed_online("host-a");
        let wave = Wave {
            hosts: vec!["host-a".into()],
            soak_minutes: 0,
        };
        let outcome = handle_wave(&fleet, &observed, &rollout, &wave, Utc::now());
        let halt_count = outcome
            .actions
            .iter()
            .filter(|a| matches!(a, Action::HaltRollout { .. }))
            .count();
        let rollback_count = outcome
            .actions
            .iter()
            .filter(|a| matches!(a, Action::RollbackHost { .. }))
            .count();
        assert_eq!(halt_count, 1, "halt expected; actions={:?}", outcome.actions);
        assert_eq!(
            rollback_count, 0,
            "no RollbackHost under `halt`; actions={:?}",
            outcome.actions
        );
    }

    #[test]
    fn failed_under_rollback_and_halt_emits_both_actions() {
        let fleet = fleet_with_policy(nixfleet_proto::OnHealthFailure::RollbackAndHalt);
        let rollout = rollout_with_state("host-a", HostRolloutState::Failed);
        let observed = observed_online("host-a");
        let wave = Wave {
            hosts: vec!["host-a".into()],
            soak_minutes: 0,
        };
        let outcome = handle_wave(&fleet, &observed, &rollout, &wave, Utc::now());
        let halt = outcome
            .actions
            .iter()
            .find(|a| matches!(a, Action::HaltRollout { .. }))
            .expect("HaltRollout still emitted");
        let rb = outcome
            .actions
            .iter()
            .find_map(|a| match a {
                Action::RollbackHost {
                    rollout,
                    host,
                    target_ref,
                } => Some((rollout.clone(), host.clone(), target_ref.clone())),
                _ => None,
            })
            .expect("RollbackHost emitted under rollback-and-halt + Failed");
        let _ = halt;
        assert_eq!(rb.0, "stable@abc12345");
        assert_eq!(rb.1, "host-a");
        assert_eq!(rb.2, "ref-xyz");
    }

    #[test]
    fn reverted_under_rollback_and_halt_does_not_re_emit_rollback() {
        let fleet = fleet_with_policy(nixfleet_proto::OnHealthFailure::RollbackAndHalt);
        let rollout = rollout_with_state("host-a", HostRolloutState::Reverted);
        let observed = observed_online("host-a");
        let wave = Wave {
            hosts: vec!["host-a".into()],
            soak_minutes: 0,
        };
        let outcome = handle_wave(&fleet, &observed, &rollout, &wave, Utc::now());
        let rollback_count = outcome
            .actions
            .iter()
            .filter(|a| matches!(a, Action::RollbackHost { .. }))
            .count();
        assert_eq!(
            rollback_count, 0,
            "Reverted suppresses RollbackHost emission; actions={:?}",
            outcome.actions
        );
    }
}
