//! Per-host state machine handling (RFC-0002 §3.2).
//!
//! Given a wave's host list, the reconciler's per-rollout state, and
//! supporting context, emit the set of actions for each host and track
//! whether the wave as a whole is soaked (all hosts in terminal ok states).

use crate::observed::{Observed, Rollout};
use crate::{budgets, edges, Action};
use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::{FleetResolved, Wave};
use std::str::FromStr;

/// RFC-0002 §3.2 per-host rollout state. The reconciler reads
/// these from `Rollout.host_states: HashMap<String, String>` (the
/// wire-shape stays stringly-typed so file-backed `observed.json`
/// fixtures keep deserialising) and parses through this enum
/// before pattern-matching.
///
/// `Queued` is the implicit default for hosts absent from the
/// `host_states` map — they have not been dispatched yet. The
/// other variants reflect the post-dispatch lifecycle the CP
/// writes into `host_rollout_state.host_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRolloutState {
    Queued,
    Dispatched,
    Activating,
    ConfirmWindow,
    Healthy,
    Soaked,
    Converged,
    Failed,
}

impl HostRolloutState {
    /// Canonical wire-string for this variant. Stays in sync with
    /// `host_rollout_state.host_state` CHECK constraint and the
    /// fixture JSON in `tests/fixtures/`.
    pub fn as_str(&self) -> &'static str {
        match self {
            HostRolloutState::Queued => "Queued",
            HostRolloutState::Dispatched => "Dispatched",
            HostRolloutState::Activating => "Activating",
            HostRolloutState::ConfirmWindow => "ConfirmWindow",
            HostRolloutState::Healthy => "Healthy",
            HostRolloutState::Soaked => "Soaked",
            HostRolloutState::Converged => "Converged",
            HostRolloutState::Failed => "Failed",
        }
    }

    /// Look up `host`'s state in `rollout.host_states`, defaulting
    /// to [`Queued`](Self::Queued) when absent. Unknown strings
    /// fall back to `Queued` so a future state name can land
    /// without panicking the reconciler — matches the existing
    /// behaviour of the `_ => {}` arm before this typed pass.
    pub fn lookup(rollout: &Rollout, host: &str) -> Self {
        rollout
            .host_states
            .get(host)
            .and_then(|s| Self::from_str(s).ok())
            .unwrap_or(HostRolloutState::Queued)
    }
}

impl FromStr for HostRolloutState {
    type Err = Error;

    /// Parse a wire-string into the typed variant. Returns an
    /// error on unknown strings — surfaces as a reconciler input
    /// error rather than silently mis-classifying a host.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Queued" => Ok(HostRolloutState::Queued),
            "Dispatched" => Ok(HostRolloutState::Dispatched),
            "Activating" => Ok(HostRolloutState::Activating),
            "ConfirmWindow" => Ok(HostRolloutState::ConfirmWindow),
            "Healthy" => Ok(HostRolloutState::Healthy),
            "Soaked" => Ok(HostRolloutState::Soaked),
            "Converged" => Ok(HostRolloutState::Converged),
            "Failed" => Ok(HostRolloutState::Failed),
            other => Err(anyhow!("unknown host_rollout_state: {other:?}")),
        }
    }
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
        let state = HostRolloutState::lookup(rollout, host);
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
            HostRolloutState::Dispatched
            | HostRolloutState::Activating
            | HostRolloutState::ConfirmWindow => {
                out.wave_all_soaked = false;
            }
            HostRolloutState::Healthy => {
                // RFC-0002 §3.2: Healthy → Soaked once the host has
                // remained Healthy for `wave.soak_minutes`. Without
                // a `last_healthy_since` marker the soak gate stays
                // closed (defensive — better to wait than promote
                // a wave that's missing data). Step 1+2 of gap #2
                // populate this map; step 3 (this arm) consumes it.
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
            HostRolloutState::Failed => {
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
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_values() {
        for v in [
            HostRolloutState::Queued,
            HostRolloutState::Dispatched,
            HostRolloutState::Activating,
            HostRolloutState::ConfirmWindow,
            HostRolloutState::Healthy,
            HostRolloutState::Soaked,
            HostRolloutState::Converged,
            HostRolloutState::Failed,
        ] {
            assert_eq!(HostRolloutState::from_str(v.as_str()).unwrap(), v);
        }
    }

    #[test]
    fn unknown_strings_error() {
        assert!(HostRolloutState::from_str("").is_err());
        assert!(HostRolloutState::from_str("queued").is_err()); // case-sensitive
        assert!(HostRolloutState::from_str("garbage").is_err());
    }

    #[test]
    fn lookup_defaults_absent_to_queued() {
        let rollout = Rollout {
            id: "r".into(),
            channel: "c".into(),
            target_ref: "ref".into(),
            state: "Executing".into(),
            current_wave: 0,
            host_states: std::collections::HashMap::new(),
            last_healthy_since: std::collections::HashMap::new(),
        };
        assert_eq!(
            HostRolloutState::lookup(&rollout, "missing"),
            HostRolloutState::Queued
        );
    }

    #[test]
    fn lookup_defaults_unknown_to_queued() {
        let mut host_states = std::collections::HashMap::new();
        host_states.insert("h".to_string(), "garbage".to_string());
        let rollout = Rollout {
            id: "r".into(),
            channel: "c".into(),
            target_ref: "ref".into(),
            state: "Executing".into(),
            current_wave: 0,
            host_states,
            last_healthy_since: std::collections::HashMap::new(),
        };
        assert_eq!(
            HostRolloutState::lookup(&rollout, "h"),
            HostRolloutState::Queued
        );
    }
}
