//! `Opening` source state. The rollout has been opened but no hosts
//! have joined yet (RFC-0012 §3).

use chrono::{DateTime, Utc};

use super::{illegal, transition_effect};
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
        // First host dispatched into the rollout → Active.
        RolloutEvent::HostJoined { wave, at, .. } => {
            let mut effects = Vec::with_capacity(2);
            effects.push(transition_effect(
                &record,
                RolloutState::Opening,
                RolloutState::Active,
                at,
            ));
            if wave != record.current_wave {
                effects.push(RolloutEffect::UpdateCurrentWave {
                    rollout_id: record.rollout_id.clone(),
                    wave,
                });
                record.current_wave = wave;
            }
            record.state = RolloutState::Active;
            Ok((record, effects))
        }

        // A successor was opened before any host joined — race condition
        // operators may encounter on a rapid manifest republish.
        RolloutEvent::SuccessorOpened { at, .. } => {
            record.state = RolloutState::Superseded;
            record.superseded_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Opening,
                RolloutState::Superseded,
                at,
            )];
            Ok((record, effects))
        }

        // OperatorClearance is rare from Opening; treat as a structural
        // no-op (record an event_log entry but no state change).
        // Wiring is out of scope for Phase 10 per brief §9.3; mark as
        // illegal so a future wiring step explicitly addresses the
        // semantic.
        RolloutEvent::OperatorClearance { .. }
        | RolloutEvent::RolloutOpened { .. }
        | RolloutEvent::HostStateChanged { .. }
        | RolloutEvent::WaveAdvanced { .. }
        | RolloutEvent::RolloutTerminal { .. }
        | RolloutEvent::RetentionExpired { .. } => Err(illegal(
            RolloutState::Opening,
            &event,
            record.rollout_id.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostState;
    use crate::rollout::state::RolloutRecord;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn opening_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Opening,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: None,
            superseded_at: None,
        }
    }

    #[test]
    fn host_joined_transitions_to_active() {
        let event = RolloutEvent::HostJoined {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            wave: 0,
            at: t0(),
        };
        let (record, effects) = step(opening_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Active);
        assert_eq!(record.current_wave, 0);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            RolloutEffect::RecordRolloutTransition {
                from: RolloutState::Opening,
                to: RolloutState::Active,
                ..
            }
        ));
    }

    #[test]
    fn host_joined_at_higher_wave_advances_wave() {
        let event = RolloutEvent::HostJoined {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            wave: 2,
            at: t0(),
        };
        let (record, effects) = step(opening_record(), event, t0()).unwrap();
        assert_eq!(record.current_wave, 2);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, RolloutEffect::UpdateCurrentWave { wave: 2, .. }))
        );
    }

    #[test]
    fn successor_opened_transitions_to_superseded() {
        let event = RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r2".into(),
            at: t0(),
        };
        let (record, _effects) = step(opening_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Superseded);
        assert_eq!(record.superseded_at, Some(t0()));
    }

    #[test]
    fn rollout_terminal_from_opening_is_illegal() {
        let event = RolloutEvent::RolloutTerminal {
            rollout_id: "r1".into(),
            at: t0(),
        };
        let err = step(opening_record(), event, t0()).unwrap_err();
        assert!(matches!(
            err,
            RolloutTransitionError::IllegalForState {
                from: RolloutState::Opening,
                ..
            }
        ));
    }

    #[test]
    fn host_state_changed_from_opening_is_illegal() {
        // No hosts have joined → can't have state changes.
        let event = RolloutEvent::HostStateChanged {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            from: HostState::Pending,
            to: HostState::Activating,
            at: t0(),
        };
        let err = step(opening_record(), event, t0()).unwrap_err();
        assert!(matches!(
            err,
            RolloutTransitionError::IllegalForState { .. }
        ));
    }
}
