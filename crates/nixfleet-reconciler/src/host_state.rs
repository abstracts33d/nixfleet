//! Per-host state machine handling (RFC-0002 §3.2).
//!
//! Given a wave's host list, the reconciler's per-rollout state, and
//! supporting context, emit the set of actions for each host and track
//! whether the wave as a whole is soaked (all hosts in terminal ok states).

use crate::observed::{Observed, Rollout};
use crate::{budgets, edges, Action};
use nixfleet_proto::FleetResolved;

pub(crate) struct WaveOutcome {
    pub actions: Vec<Action>,
    pub wave_all_soaked: bool,
}

pub(crate) fn handle_wave(
    fleet: &FleetResolved,
    observed: &Observed,
    rollout: &Rollout,
    wave_hosts: &[String],
) -> WaveOutcome {
    let mut out = WaveOutcome {
        actions: Vec::new(),
        wave_all_soaked: true,
    };

    for host in wave_hosts {
        let state = rollout
            .host_states
            .get(host)
            .map(String::as_str)
            .unwrap_or("Queued");
        match state {
            "Queued" => {
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
                if let Some((in_flight, max)) = budgets::budget_max(fleet, observed, host) {
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
            "Dispatched" | "Activating" | "ConfirmWindow" | "Healthy" => {
                out.wave_all_soaked = false;
            }
            "Soaked" | "Converged" => {}
            "Failed" => {
                out.wave_all_soaked = false;
                if let Some(chan) = fleet.channels.get(&rollout.channel) {
                    if let Some(policy) = fleet.rollout_policies.get(&chan.rollout_policy) {
                        out.actions.push(Action::HaltRollout {
                            rollout: rollout.id.clone(),
                            reason: format!(
                                "host {host} failed (policy: {})",
                                policy.on_health_failure
                            ),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    out
}
