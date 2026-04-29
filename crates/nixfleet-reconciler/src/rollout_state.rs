//! Rollout-level state machine handling (RFC-0002 §3.1).

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

/// RFC-0002 §3.1 rollout-level state. Persisted on the wire as a
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
        // Issue #59 / #60 — wave-promotion gate. Before advancing
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
        let blocked_hosts: Vec<String> = if channel_mode.is_enforcing() {
            // Hosts on this rollout's waves up-to-and-including the
            // current wave. Wave promotion is to current_wave + 1,
            // so an outstanding event on any host in [0..=current]
            // holds the promotion.
            let mut out = Vec::new();
            for wave_idx in 0..=rollout.current_wave {
                if let Some(w) = waves.get(wave_idx) {
                    for host in &w.hosts {
                        if observed
                            .host_compliance_failures
                            .get(host)
                            .copied()
                            .unwrap_or(0)
                            > 0
                        {
                            out.push(host.clone());
                        }
                    }
                }
            }
            out.sort_unstable();
            out.dedup();
            out
        } else {
            Vec::new()
        };

        if !blocked_hosts.is_empty() {
            let total: usize = blocked_hosts
                .iter()
                .map(|h| {
                    observed
                        .host_compliance_failures
                        .get(h)
                        .copied()
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
}
