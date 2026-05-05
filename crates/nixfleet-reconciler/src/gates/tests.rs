//! Parity tests for the gates registry.
//!
//! Each gate gets a positive case (gate fires) and a negative case (gate
//! passes) verified against `evaluate_for_host`. These prove the
//! behaviour-of-record at the registry level — if you add a gate, you
//! add a parity test here. CP-side parity (reconciler emits Skip,
//! dispatch endpoint returns None for the same Observed) is enforced
//! by integration tests in nixfleet-control-plane.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use nixfleet_proto::{
    Channel, ChannelEdge, Compliance, Edge, FleetResolved, Host, Meta, OnHealthFailure,
    PolicyWave, RolloutBudget, RolloutPolicy, Selector, Wave,
};

use crate::host_state::HostRolloutState;
use crate::observed::{Observed, Rollout};
use crate::rollout_state::RolloutState;

use super::{evaluate_for_host, GateBlock, GateInput};

fn empty_set() -> HashSet<String> {
    HashSet::new()
}

fn fleet_two_channels() -> FleetResolved {
    let mut hosts = HashMap::new();
    hosts.insert(
        "lab".into(),
        Host {
            system: "x86_64-linux".into(),
            tags: vec!["server".into()],
            channel: "edge".into(),
            closure_hash: Some("lab-closure".into()),
            pubkey: None,
        },
    );
    hosts.insert(
        "krach".into(),
        Host {
            system: "x86_64-linux".into(),
            tags: vec!["dev".into()],
            channel: "stable".into(),
            closure_hash: Some("krach-closure".into()),
            pubkey: None,
        },
    );
    hosts.insert(
        "aether".into(),
        Host {
            system: "aarch64-darwin".into(),
            tags: vec!["dev".into()],
            channel: "stable".into(),
            closure_hash: Some("aether-closure".into()),
            pubkey: None,
        },
    );

    let mut channels = HashMap::new();
    for ch in ["edge", "stable"] {
        channels.insert(
            ch.into(),
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
            strategy: "staged".into(),
            waves: vec![PolicyWave {
                selector: Selector {
                    tags: vec!["dev".into()],
                    ..Default::default()
                },
                soak_minutes: 5,
            }],
            health_gate: Default::default(),
            on_health_failure: OnHealthFailure::Halt,
        },
    );

    let mut waves = HashMap::new();
    waves.insert(
        "stable".into(),
        vec![Wave {
            hosts: vec!["krach".into(), "aether".into()],
            soak_minutes: 5,
        }],
    );

    FleetResolved {
        schema_version: 1,
        hosts,
        channels,
        rollout_policies,
        waves,
        edges: vec![],
        channel_edges: vec![ChannelEdge {
            before: "edge".into(),
            after: "stable".into(),
            reason: None,
        }],
        disruption_budgets: vec![],
        meta: Meta {
            schema_version: 1,
            signed_at: None,
            ci_commit: None,
            signature_algorithm: Some("ed25519".into()),
        },
    }
}

fn rollout(channel: &str, host_states: Vec<(&str, HostRolloutState)>) -> Rollout {
    Rollout {
        id: format!("rid-{channel}"),
        channel: channel.into(),
        target_ref: "ref".into(),
        state: RolloutState::Executing,
        current_wave: 0,
        host_states: host_states
            .into_iter()
            .map(|(h, s)| (h.to_string(), s))
            .collect(),
        last_healthy_since: HashMap::new(),
        budgets: vec![],
    }
}

#[test]
fn channel_edges_blocks_when_predecessor_active() {
    let fleet = fleet_two_channels();
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Activating)],
        )],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: None,
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(
        evaluate_for_host(&input),
        Some(GateBlock::ChannelEdges {
            predecessor_channel: "edge".into(),
        }),
    );
}

#[test]
fn channel_edges_passes_when_predecessor_converged() {
    let fleet = fleet_two_channels();
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: None,
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(evaluate_for_host(&input), None);
}

#[test]
fn channel_edges_conservative_blocks_on_missing_predecessor_with_hosts() {
    // Fresh-boot scenario: predecessor channel has hosts in fleet but
    // no rollout recorded yet. Dispatch endpoint sets
    // conservative_on_missing_state=true to block until polling
    // populates state.
    let fleet = fleet_two_channels();
    let observed = Observed::default();
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: None,
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: true,
    };
    assert_eq!(
        evaluate_for_host(&input),
        Some(GateBlock::ChannelEdges {
            predecessor_channel: "edge".into(),
        }),
    );
}

#[test]
fn wave_promotion_blocks_wave_one_when_current_is_zero() {
    let mut fleet = fleet_two_channels();
    // Add a second wave for stable so krach is wave 0, aether is wave 1.
    fleet.waves.insert(
        "stable".into(),
        vec![
            Wave {
                hosts: vec!["krach".into()],
                soak_minutes: 5,
            },
            Wave {
                hosts: vec!["aether".into()],
                soak_minutes: 60,
            },
        ],
    );
    let r = rollout("stable", vec![]);
    assert_eq!(r.current_wave, 0);
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "aether",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(
        evaluate_for_host(&input),
        Some(GateBlock::WavePromotion {
            host_wave: 1,
            current_wave: 0,
        }),
    );
}

#[test]
fn host_edges_blocks_until_gating_host_converges() {
    // fleet.edges = [{ gated: krach, gates: aether }]
    // krach's dispatch is held until aether reaches Soaked/Converged.
    let mut fleet = fleet_two_channels();
    fleet.edges = vec![Edge {
        gated: "krach".into(),
        gates: "aether".into(),
        reason: None,
    }];
    let r = rollout(
        "stable",
        vec![("aether", HostRolloutState::Activating)], // aether not yet Soaked/Converged
    );
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(
        evaluate_for_host(&input),
        Some(GateBlock::HostEdge {
            gating_host: "aether".into(),
        }),
    );
}

#[test]
fn disruption_budget_blocks_when_at_max_in_flight() {
    let fleet = fleet_two_channels();
    let dev_selector = Selector {
        tags: vec!["dev".into()],
        ..Default::default()
    };
    let budgets = vec![RolloutBudget {
        selector: dev_selector,
        hosts: vec!["krach".into(), "aether".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    }];
    let mut r = rollout("stable", vec![("krach", HostRolloutState::Healthy)]);
    r.budgets = budgets.clone();
    let observed = Observed {
        active_rollouts: vec![r.clone()],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "aether",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    let block = evaluate_for_host(&input);
    match block {
        Some(GateBlock::DisruptionBudget { in_flight, max, .. }) => {
            assert_eq!(in_flight, 1);
            assert_eq!(max, 1);
        }
        other => panic!("expected DisruptionBudget block, got {other:?}"),
    }
}

#[test]
fn disruption_budget_passes_when_under_max() {
    let fleet = fleet_two_channels();
    let dev_selector = Selector {
        tags: vec!["dev".into()],
        ..Default::default()
    };
    let budgets = vec![RolloutBudget {
        selector: dev_selector,
        hosts: vec!["krach".into(), "aether".into()],
        max_in_flight: Some(2),
        max_in_flight_pct: None,
    }];
    let mut r = rollout("stable", vec![("krach", HostRolloutState::Healthy)]);
    r.budgets = budgets;
    let observed = Observed {
        active_rollouts: vec![r.clone()],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "aether",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(evaluate_for_host(&input), None);
}

#[test]
fn host_edges_skips_cross_channel_edges() {
    // Regression: Edge { before: krach (stable), after: lab (edge) }
    // would look up lab in stable's rollout.host_states (always None),
    // default to Queued, and block krach forever. The cross-channel
    // guard treats such edges as no-ops.
    let mut fleet = fleet_two_channels();
    fleet.edges = vec![Edge {
        gated: "krach".into(),
        gates: "lab".into(),
        reason: None,
    }];
    let r = rollout("stable", vec![]);
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(
        evaluate_for_host(&input),
        None,
        "cross-channel host edge must NOT block",
    );
}

#[test]
fn compliance_wave_blocks_when_earlier_wave_has_failures_under_enforce() {
    let mut fleet = fleet_two_channels();
    fleet
        .channels
        .get_mut("stable")
        .unwrap()
        .compliance
        .mode = "enforce".into();
    fleet.waves.insert(
        "stable".into(),
        vec![
            Wave {
                hosts: vec!["krach".into()],
                soak_minutes: 5,
            },
            Wave {
                hosts: vec!["aether".into()],
                soak_minutes: 60,
            },
        ],
    );

    let mut r = rollout("stable", vec![]);
    r.current_wave = 1; // wave_promotion gate must pass for aether (wave 1)
    let mut compliance_failures = HashMap::new();
    let mut by_host = HashMap::new();
    by_host.insert("krach".to_string(), 2usize);
    compliance_failures.insert(r.id.clone(), by_host);

    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        compliance_failures_by_rollout: compliance_failures,
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "aether",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    let block = evaluate_for_host(&input);
    match block {
        Some(GateBlock::ComplianceWave {
            failing_events_count,
            host_wave,
        }) => {
            assert_eq!(failing_events_count, 2);
            assert_eq!(host_wave, 1);
        }
        other => panic!("expected ComplianceWave block, got {other:?}"),
    }
}

#[test]
fn compliance_wave_passes_under_permissive_mode() {
    let mut fleet = fleet_two_channels();
    fleet
        .channels
        .get_mut("stable")
        .unwrap()
        .compliance
        .mode = "permissive".into();
    fleet.waves.insert(
        "stable".into(),
        vec![
            Wave {
                hosts: vec!["krach".into()],
                soak_minutes: 5,
            },
            Wave {
                hosts: vec!["aether".into()],
                soak_minutes: 60,
            },
        ],
    );

    let mut r = rollout("stable", vec![]);
    r.current_wave = 1; // wave_promotion gate must pass for aether (wave 1)
    let mut compliance_failures = HashMap::new();
    let mut by_host = HashMap::new();
    by_host.insert("krach".to_string(), 5usize);
    compliance_failures.insert(r.id.clone(), by_host);

    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        compliance_failures_by_rollout: compliance_failures,
        ..Default::default()
    };
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "aether",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(
        evaluate_for_host(&input),
        None,
        "permissive mode must not block",
    );
}

#[test]
fn empty_input_passes_all_gates() {
    let fleet = fleet_two_channels();
    let observed = Observed {
        active_rollouts: vec![rollout(
            "edge",
            vec![("lab", HostRolloutState::Converged)],
        )],
        ..Default::default()
    };
    let r = rollout("stable", vec![]);
    let empty = empty_set();
    let input = GateInput {
        fleet: &fleet,
        observed: &observed,
        rollout: Some(&r),
        host: "krach",
        now: Utc::now(),
        emitted_opens_in_tick: &empty,
        conservative_on_missing_state: false,
    };
    assert_eq!(evaluate_for_host(&input), None);
}
