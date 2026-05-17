//! `Converging` source state. All current-wave hosts have reached
//! Soaking or beyond; later waves remain to dispatch (or, with no
//! more waves, all hosts await Converged).

use chrono::{DateTime, Utc};

use super::{illegal, transition_effect};
use crate::HostState;
use crate::rollout::effect::RolloutEffect;
use crate::rollout::error::RolloutTransitionError;
use crate::rollout::event::RolloutEvent;
use crate::rollout::state::{RolloutRecord, RolloutState};

pub(super) fn step(
    mut record: RolloutRecord,
    event: RolloutEvent,
    _now: DateTime<Utc>,
) -> Result<(RolloutRecord, Vec<RolloutEffect>), RolloutTransitionError> {
    match event {
        // Next wave dispatched → back to Active.
        RolloutEvent::WaveAdvanced {
            from_wave,
            to_wave,
            at,
            ..
        } if to_wave > from_wave => {
            record.state = RolloutState::Active;
            let mut effects = Vec::new();
            effects.push(transition_effect(
                &record,
                RolloutState::Converging,
                RolloutState::Active,
                at,
            ));
            // Monotonic — never roll back current_wave.
            if to_wave > record.current_wave {
                record.current_wave = to_wave;
                effects.push(RolloutEffect::UpdateCurrentWave {
                    rollout_id: record.rollout_id.clone(),
                    wave: to_wave,
                });
            }
            Ok((record, effects))
        }

        // Idempotent: already-Converging WaveAdvanced for the same wave.
        RolloutEvent::WaveAdvanced { .. } => Ok((record, Vec::new())),

        // All hosts in all waves Converged.
        RolloutEvent::RolloutTerminal { at, .. } => {
            record.state = RolloutState::Terminal;
            record.terminal_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Converging,
                RolloutState::Terminal,
                at,
            )];
            Ok((record, effects))
        }

        // A host slipped back from Soaking to Failed/Reverted late.
        RolloutEvent::HostStateChanged { to, at, .. } => match to {
            HostState::Reverted => {
                record.state = RolloutState::Reverted;
                let effects = vec![transition_effect(
                    &record,
                    RolloutState::Converging,
                    RolloutState::Reverted,
                    at,
                )];
                Ok((record, effects))
            }
            HostState::Failed => {
                record.state = RolloutState::Failed;
                let effects = vec![transition_effect(
                    &record,
                    RolloutState::Converging,
                    RolloutState::Failed,
                    at,
                )];
                Ok((record, effects))
            }
            _ => Ok((record, Vec::new())),
        },

        RolloutEvent::SuccessorOpened { at, .. } => {
            record.state = RolloutState::Superseded;
            record.superseded_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Converging,
                RolloutState::Superseded,
                at,
            )];
            Ok((record, effects))
        }

        // HostJoined on a later wave bumps current_wave but stays
        // Converging until WaveAdvanced fires.
        RolloutEvent::HostJoined { wave, .. } if wave > record.current_wave => {
            record.current_wave = wave;
            Ok((
                record.clone(),
                vec![RolloutEffect::UpdateCurrentWave {
                    rollout_id: record.rollout_id,
                    wave,
                }],
            ))
        }
        RolloutEvent::HostJoined { .. } => Ok((record, Vec::new())),

        RolloutEvent::RolloutOpened { .. }
        | RolloutEvent::RetentionExpired { .. }
        | RolloutEvent::OperatorClearance { .. } => Err(illegal(
            RolloutState::Converging,
            &event,
            record.rollout_id.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn converging_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Converging,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: None,
            superseded_at: None,
        }
    }

    #[test]
    fn wave_advanced_to_higher_returns_to_active() {
        let event = RolloutEvent::WaveAdvanced {
            rollout_id: "r1".into(),
            from_wave: 0,
            to_wave: 1,
            at: t0(),
        };
        let (record, _) = step(converging_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Active);
        assert_eq!(record.current_wave, 1);
    }

    #[test]
    fn rollout_terminal_transitions() {
        let event = RolloutEvent::RolloutTerminal {
            rollout_id: "r1".into(),
            at: t0(),
        };
        let (record, _) = step(converging_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Terminal);
        assert_eq!(record.terminal_at, Some(t0()));
    }

    #[test]
    fn late_host_failed_transitions_to_failed() {
        let event = RolloutEvent::HostStateChanged {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            from: HostState::Soaking,
            to: HostState::Failed,
            at: t0(),
        };
        let (record, _) = step(converging_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Failed);
    }
}
