//! Rollout-level state machine handling .

use crate::host_state::{self, WaveOutcome};
use crate::observed::{Observed, Rollout};
use crate::Action;
use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;
use std::str::FromStr;

// `FromStr` is still used by the proto-side / DB-side string
// boundary; keep it on the public API even though the reconciler
// itself now reads `Rollout.state` as the typed variant directly.

/// rollout-level state. Persisted on the wire as a
/// string in `Rollout.state` JSON (a serde shim on the struct
/// round-trips through [`Self::as_str`] / [`Self::from_str`] so
/// fixtures stay byte-identical). Lifecycle:
///
/// - [`Planning`](Self::Planning): rollout opened but not yet
///   executing — reserved; the current CP transitions Planning →
///   Executing inline so callers rarely observe this variant.
/// - [`Executing`](Self::Executing): the reconciler advances waves
///   and emits per-host actions.
/// - [`Halted`](Self::Halted): a `HaltRollout` action fired (e.g.
///   a host entered Failed under a halt-on-failure policy). The
///   reconciler stops advancing and waits for operator action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RolloutState {
    Planning,
    Executing,
    Halted,
}

impl RolloutState {
    /// Canonical wire-string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            RolloutState::Planning => "Planning",
            RolloutState::Executing => "Executing",
            RolloutState::Halted => "Halted",
        }
    }
}

impl FromStr for RolloutState {
    type Err = Error;

    /// Parse a wire-string into the typed variant. Returns an
    /// error on unknown strings.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Planning" => Ok(RolloutState::Planning),
            "Executing" => Ok(RolloutState::Executing),
            "Halted" => Ok(RolloutState::Halted),
            other => Err(anyhow!("unknown rollout state: {other:?}")),
        }
    }
}

pub(crate) fn advance_rollout(
    fleet: &FleetResolved,
    observed: &Observed,
    rollout: &Rollout,
    now: DateTime<Utc>,
) -> Vec<Action> {
    let mut actions = Vec::new();

    // Only advance when Executing. Planning / Halted: nothing to do
    // — reconciler waits for the next state transition.
    if rollout.state != RolloutState::Executing {
        return actions;
    }

    let waves = match fleet.waves.get(&rollout.channel) {
        Some(w) => w,
        None => return actions, // missing-channel: silent continue (spec OQ #5)
    };
    let wave = match waves.get(rollout.current_wave) {
        Some(w) => w,
        None => {
            actions.push(Action::ConvergeRollout {
                rollout: rollout.id.clone(),
            });
            return actions;
        }
    };

    let WaveOutcome {
        actions: wave_actions,
        wave_all_soaked,
    } = host_state::handle_wave(fleet, observed, rollout, wave, now);
    actions.extend(wave_actions);

    if wave_all_soaked {
        // — wave-promotion gate. Before advancing
        // wave N → N+1, check the durable host_reports projection
        // for outstanding ComplianceFailure / RuntimeGateError
        // events on hosts in waves 0..=N. Channel mode `enforce`
        // converts the check into an `Action::WaveBlocked` that
        // takes the place of `PromoteWave`. `permissive` /
        // `disabled` ignore the events for gating but the events
        // still flow to operators via the report log.
        let channel_mode = fleet
            .channels
            .get(&rollout.channel)
            .map(|c| nixfleet_proto::compliance::GateMode::from_wire_str(&c.compliance.mode))
            .unwrap_or(nixfleet_proto::compliance::GateMode::Disabled);
        // Resolution-by-replacement is enforced at the projection
        // layer: events bound to a different rollout id don't appear
        // under THIS rollout's key, so iterating waves[0..=current]
        // never picks up stale failures from a rollout the host
        // has moved past.
        let per_host = observed
            .compliance_failures_by_rollout
            .get(&rollout.id);
        let blocked_hosts: Vec<String> = if channel_mode.is_enforcing() {
            let mut out = Vec::new();
            if let Some(map) = per_host {
                for wave_idx in 0..=rollout.current_wave {
                    if let Some(w) = waves.get(wave_idx) {
                        for host in &w.hosts {
                            if map.get(host).copied().unwrap_or(0) > 0 {
                                out.push(host.clone());
                            }
                        }
                    }
                }
                out.sort_unstable();
                out.dedup();
            }
            out
        } else {
            Vec::new()
        };

        if !blocked_hosts.is_empty() {
            let total: usize = blocked_hosts
                .iter()
                .map(|h| {
                    per_host
                        .and_then(|m| m.get(h).copied())
                        .unwrap_or(0)
                })
                .sum();
            actions.push(Action::WaveBlocked {
                rollout: rollout.id.clone(),
                blocked_wave: rollout.current_wave + 1,
                failing_hosts: blocked_hosts,
                failing_events_count: total,
            });
        } else if rollout.current_wave + 1 >= waves.len() {
            actions.push(Action::ConvergeRollout {
                rollout: rollout.id.clone(),
            });
        } else {
            actions.push(Action::PromoteWave {
                rollout: rollout.id.clone(),
                new_wave: rollout.current_wave + 1,
            });
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_values() {
        for v in [
            RolloutState::Planning,
            RolloutState::Executing,
            RolloutState::Halted,
        ] {
            assert_eq!(RolloutState::from_str(v.as_str()).unwrap(), v);
        }
    }

    #[test]
    fn unknown_strings_error() {
        assert!(RolloutState::from_str("").is_err());
        assert!(RolloutState::from_str("executing").is_err()); // case-sensitive
        assert!(RolloutState::from_str("garbage").is_err());
    }

    // ===============================================================
    // Action::WaveBlocked emission — integration coverage
    // ===============================================================
    //
    // The wave-staging gate's per-rollout, per-host filtering needs
    // tests at this layer because the broken-shape projection (a
    // global host count instead of per-rollout) would have been
    // accepted by the parser-level tests but caused stale events
    // from old rollouts to block fresh wave promotion forever.
    // These tests exercise the FULL signal path:
    // Observed.compliance_failures_by_rollout
    // → advance_rollout iterates the right rollout
    // → emits the right action (WaveBlocked vs PromoteWave vs
    // ConvergeRollout) per channel mode + per-rollout state.

    use crate::host_state::HostRolloutState;
    use crate::observed::{Observed, Rollout};
    use chrono::Utc;
    use nixfleet_proto::{
        fleet_resolved::{
            Channel, Compliance, HealthGate, Host, Meta, OnHealthFailure, PolicyWave,
            RolloutPolicy, Selector, Wave,
        },
        FleetResolved,
    };
    use std::collections::HashMap;

    /// Build a fleet with one channel `stable`, two waves (`host-a`
    /// in wave 0; `host-b` in wave 1), and `compliance.mode` of the
    /// caller's choice.
    fn fleet_two_waves(compliance_mode: &str) -> FleetResolved {
        let mut hosts = HashMap::new();
        hosts.insert(
            "host-a".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: Some("c-a".into()),
                pubkey: None,
            },
        );
        hosts.insert(
            "host-b".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: Some("c-b".into()),
                pubkey: None,
            },
        );
        let mut channels = HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "default".into(),
                reconcile_interval_minutes: 30,
                freshness_window: 720,
                signing_interval_minutes: 30,
                compliance: Compliance {
                    mode: compliance_mode.to_string(),
                    frameworks: vec![],
                },
            },
        );
        let mut rollout_policies = HashMap::new();
        rollout_policies.insert(
            "default".to_string(),
            RolloutPolicy {
                strategy: "staged".into(),
                waves: vec![
                    PolicyWave {
                        selector: Selector {
                            tags: vec![],
                            tags_any: vec![],
                            hosts: vec!["host-a".into()],
                            channel: None,
                            all: false,
                        },
                        soak_minutes: 0,
                    },
                    PolicyWave {
                        selector: Selector {
                            tags: vec![],
                            tags_any: vec![],
                            hosts: vec!["host-b".into()],
                            channel: None,
                            all: false,
                        },
                        soak_minutes: 0,
                    },
                ],
                health_gate: HealthGate::default(),
                on_health_failure: OnHealthFailure::Halt,
            },
        );
        let mut waves = HashMap::new();
        waves.insert(
            "stable".to_string(),
            vec![
                Wave {
                    hosts: vec!["host-a".into()],
                    soak_minutes: 0,
                },
                Wave {
                    hosts: vec!["host-b".into()],
                    soak_minutes: 0,
                },
            ],
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies,
            waves,
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: Some(Utc::now()),
                ci_commit: Some("abc12345".into()),
                signature_algorithm: None,
            },
        }
    }

    /// Build a Rollout in Executing state at wave 0, with both
    /// hosts in `Soaked` (so `wave_all_soaked` is true and the
    /// gate decision fires).
    fn rollout_at_wave_0_soaked(id: &str) -> Rollout {
        let mut host_states = HashMap::new();
        host_states.insert("host-a".into(), HostRolloutState::Soaked);
        Rollout {
            id: id.into(),
            channel: "stable".into(),
            target_ref: id.into(),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states,
            last_healthy_since: HashMap::new(),
        }
    }

    fn observed_with_failures(
        rollout_id: &str,
        failures: &[(&str, usize)],
    ) -> Observed {
        let mut by_rollout = HashMap::new();
        let mut per_host = HashMap::new();
        for (h, n) in failures {
            per_host.insert(h.to_string(), *n);
        }
        if !per_host.is_empty() {
            by_rollout.insert(rollout_id.to_string(), per_host);
        }
        Observed {
            channel_refs: HashMap::new(),
            last_rolled_refs: HashMap::new(),
            host_state: HashMap::new(),
            active_rollouts: vec![],
            compliance_failures_by_rollout: by_rollout,
        }
    }

    fn extract_action_kind(actions: &[Action]) -> Vec<&'static str> {
        actions
            .iter()
            .map(|a| match a {
                Action::OpenRollout { .. } => "open_rollout",
                Action::DispatchHost { .. } => "dispatch_host",
                Action::PromoteWave { .. } => "promote_wave",
                Action::ConvergeRollout { .. } => "converge_rollout",
                Action::HaltRollout { .. } => "halt_rollout",
                Action::SoakHost { .. } => "soak_host",
                Action::ChannelUnknown { .. } => "channel_unknown",
                Action::Skip { .. } => "skip",
                Action::WaveBlocked { .. } => "wave_blocked",
            })
            .collect()
    }

    #[test]
    fn wave_blocked_emits_when_enforce_and_outstanding_event_for_this_rollout() {
        let fleet = fleet_two_waves("enforce");
        let rollout = rollout_at_wave_0_soaked("R1");
        // host-a (wave 0) has an outstanding event posted under R1
        // — this is THIS rollout's id, so the gate fires.
        let observed = observed_with_failures("R1", &[("host-a", 1)]);
        let actions = advance_rollout(&fleet, &observed, &rollout, Utc::now());
        let kinds = extract_action_kind(&actions);
        assert!(
            kinds.contains(&"wave_blocked"),
            "expected WaveBlocked, got {kinds:?}",
        );
        assert!(
            !kinds.contains(&"promote_wave"),
            "WaveBlocked must replace PromoteWave",
        );
        // Verify shape: blocked_wave is current+1 (1), failing_hosts
        // contains host-a, total = 1.
        let wb = actions
            .iter()
            .find_map(|a| match a {
                Action::WaveBlocked {
                    rollout,
                    blocked_wave,
                    failing_hosts,
                    failing_events_count,
                } => Some((rollout, *blocked_wave, failing_hosts, *failing_events_count)),
                _ => None,
            })
            .expect("WaveBlocked emitted");
        assert_eq!(wb.0, "R1");
        assert_eq!(wb.1, 1);
        assert_eq!(wb.2, &vec!["host-a".to_string()]);
        assert_eq!(wb.3, 1);
    }

    #[test]
    fn wave_blocked_does_not_emit_for_event_bound_to_different_rollout() {
        // **Resolution-by-replacement test.** host-a has an event in
        // the projection — but it's bound to R0. We're advancing R1.
        // The reconciler must NOT block R1's promotion on R0's stale
        // events. This is the bug the projection-layer per-rollout
        // grouping was supposed to prevent — without it, a 6-month-old
        // R0 event would block R1 forever.
        let fleet = fleet_two_waves("enforce");
        let rollout = rollout_at_wave_0_soaked("R1");
        let observed = observed_with_failures("R0", &[("host-a", 1)]);
        let actions = advance_rollout(&fleet, &observed, &rollout, Utc::now());
        let kinds = extract_action_kind(&actions);
        assert!(
            kinds.contains(&"promote_wave"),
            "expected PromoteWave (R0 events do not block R1), got {kinds:?}",
        );
        assert!(
            !kinds.contains(&"wave_blocked"),
            "stale R0 events must not block R1 — resolution-by-replacement",
        );
    }

    #[test]
    fn wave_blocked_does_not_emit_under_permissive_mode() {
        // Permissive: events post + gate observes them but never
        // blocks dispatch / wave promotion. The PromoteWave proceeds.
        let fleet = fleet_two_waves("permissive");
        let rollout = rollout_at_wave_0_soaked("R1");
        let observed = observed_with_failures("R1", &[("host-a", 1)]);
        let actions = advance_rollout(&fleet, &observed, &rollout, Utc::now());
        let kinds = extract_action_kind(&actions);
        assert!(
            kinds.contains(&"promote_wave"),
            "permissive mode advances regardless, got {kinds:?}",
        );
        assert!(!kinds.contains(&"wave_blocked"));
    }

    #[test]
    fn wave_blocked_does_not_emit_under_disabled_mode() {
        let fleet = fleet_two_waves("disabled");
        let rollout = rollout_at_wave_0_soaked("R1");
        let observed = observed_with_failures("R1", &[("host-a", 1)]);
        let actions = advance_rollout(&fleet, &observed, &rollout, Utc::now());
        let kinds = extract_action_kind(&actions);
        assert!(kinds.contains(&"promote_wave"));
        assert!(!kinds.contains(&"wave_blocked"));
    }

    #[test]
    fn wave_blocked_aggregates_multiple_hosts_in_earlier_waves() {
        // current_wave=1; both wave-0 hosts have outstanding events
        // for THIS rollout. Both should appear in failing_hosts;
        // total event count is the sum.
        let mut fleet = fleet_two_waves("enforce");
        // Make wave 0 carry both hosts so we can populate failures
        // on both at the same wave-index.
        let waves_for_stable = fleet.waves.get_mut("stable").unwrap();
        waves_for_stable[0].hosts = vec!["host-a".into(), "host-b".into()];
        let mut rollout = rollout_at_wave_0_soaked("R1");
        rollout
            .host_states
            .insert("host-b".into(), HostRolloutState::Soaked);
        let observed =
            observed_with_failures("R1", &[("host-a", 2), ("host-b", 3)]);
        let actions = advance_rollout(&fleet, &observed, &rollout, Utc::now());
        let wb = actions
            .iter()
            .find_map(|a| match a {
                Action::WaveBlocked {
                    failing_hosts,
                    failing_events_count,
                    ..
                } => Some((failing_hosts, *failing_events_count)),
                _ => None,
            })
            .expect("WaveBlocked emitted");
        assert_eq!(wb.0, &vec!["host-a".to_string(), "host-b".to_string()]);
        assert_eq!(wb.1, 5);
    }
}

