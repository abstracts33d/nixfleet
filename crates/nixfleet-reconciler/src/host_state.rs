//! Per-host state machine handling .
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

/// per-host rollout state. The reconciler reads
/// these from `Rollout.host_states: HashMap<String, HostRolloutState>`
/// (a serde shim on the wire keeps file-backed `observed.json`
/// fixtures byte-identical) and pattern-matches directly.
///
/// `Queued` is the implicit default for hosts absent from the
/// `host_states` map — they have not been dispatched yet. The
/// other variants reflect the post-dispatch lifecycle the CP
/// writes into `host_rollout_state.host_state`.
///
/// The variant set is the canonical truth for the
/// `host_rollout_state.host_state` SQL CHECK constraint
/// (V003__host_rollout_state.sql). Adding a value here without
/// extending the CHECK (or vice versa) lets `from_str` reject
/// rows the SQL accepted, and is caught by the
/// `host_rollout_state_check_matches_enum` test in the CP crate.
/// `Reverted` is currently dormant: V003 reserves the wire string
/// for the explicit-rollback path that lands with the rollout-
/// halt action handler. The variant exists so the typed
/// projection round-trips it instead of silently mapping to
/// `Queued` (which would re-dispatch the host into a loop
/// the inverse of resolution-by-replacement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRolloutState {
    Queued,
    Dispatched,
    Activating,
    ConfirmWindow,
    Healthy,
    Soaked,
    Converged,
    Reverted,
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
            HostRolloutState::Reverted => "Reverted",
            HostRolloutState::Failed => "Failed",
        }
    }

    /// Look up `host`'s state in `rollout.host_states`, defaulting
    /// to [`Queued`](Self::Queued) when absent. Hosts not yet
    /// dispatched have no row in `host_rollout_state`; the
    /// reconciler treats them as fresh Queued work.
    pub fn lookup(rollout: &Rollout, host: &str) -> Self {
        rollout
            .host_states
            .get(host)
            .copied()
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
            "Reverted" => Ok(HostRolloutState::Reverted),
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
                // : Healthy → Soaked once the host has
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
            HostRolloutState::Failed | HostRolloutState::Reverted => {
                // Both states block wave-soaking and halt the rollout
                // per the channel's `on_health_failure` policy. The
                // distinction is provenance: `Failed` is reconciler-
                // observed (probe failure, exit-code != 0); `Reverted`
                // is agent-attested (the host explicitly rolled back
                // its activation). The downstream halt action treats
                // them the same — the rollout is unsafe to advance
                // until an operator inspects.
                out.wave_all_soaked = false;
                if let Some(chan) = fleet.channels.get(&rollout.channel) {
                    if let Some(policy) = fleet.rollout_policies.get(&chan.rollout_policy) {
                        out.actions.push(Action::HaltRollout {
                            rollout: rollout.id.clone(),
                            reason: format!(
                                "host {host} {} (policy: {})",
                                state.as_str().to_lowercase(),
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
            HostRolloutState::Reverted,
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
        use crate::rollout_state::RolloutState;
        let rollout = Rollout {
            id: "r".into(),
            channel: "c".into(),
            target_ref: "ref".into(),
            state: RolloutState::Executing,
            current_wave: 0,
            host_states: std::collections::HashMap::new(),
            last_healthy_since: std::collections::HashMap::new(),
        };
        assert_eq!(
            HostRolloutState::lookup(&rollout, "missing"),
            HostRolloutState::Queued
        );
    }
}
