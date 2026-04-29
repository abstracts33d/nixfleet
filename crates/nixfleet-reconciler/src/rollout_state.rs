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
        if rollout.current_wave + 1 >= waves.len() {
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
