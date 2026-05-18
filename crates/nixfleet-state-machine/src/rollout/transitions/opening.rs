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
        //
        // LOADBEARING: `wave` is recorded in the event for event_log
        // audit / replay reconstruction (which wave was the first
        // joiner) but does NOT mutate `record.current_wave`. The
        // wave-promotion gate reads `current_wave` as the "wave
        // cursor for which dispatches are currently allowed" per
        // RFC-0012 §6.3; bumping the cursor on HostJoined would leak
        // it forward — wave-N+1 hosts dispatching alongside wave-N
        // hosts on the first plan tick would set the cursor to
        // max-of-joiners' wave_index, passing wave-N+1 vacuously.
        // The cursor advances ONLY via deliberate progression:
        // `advance_current_waves` in the reducer when every host in
        // `current_wave` reaches Converged → emits `WaveAdvanced` →
        // the active.rs transition bumps the cursor.
        RolloutEvent::HostJoined { at, .. } => {
            record.state = RolloutState::Active;
            Ok((
                record.clone(),
                vec![transition_effect(
                    &record,
                    RolloutState::Opening,
                    RolloutState::Active,
                    at,
                )],
            ))
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

        // OperatorClearance from Opening would be a no-op (record an
        // event_log entry but no state change). Wiring deferred per
        // v0.2.1-followups; marked illegal here so a future wiring
        // step explicitly addresses the semantic instead of silently
        // accepting it.
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

    /// Regression pin: HostJoined MUST NOT mutate `current_wave`.
    /// Assigning the joiner's `wave` to the cursor would leak the
    /// wave-promotion cursor forward when multi-wave rollouts have
    /// wave-N+1 hosts joining on the first plan tick — the cursor
    /// must stay at the canonical initial value (0) so the
    /// wave-promotion gate correctly blocks higher-wave dispatches.
    #[test]
    fn host_joined_does_not_mutate_current_wave_even_at_higher_wave_index() {
        let event = RolloutEvent::HostJoined {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            wave: 2,
            at: t0(),
        };
        let (record, effects) = step(opening_record(), event, t0()).unwrap();
        assert_eq!(
            record.current_wave, 0,
            "HostJoined MUST NOT bump current_wave — the wave cursor advances ONLY via advance_current_waves + WaveAdvanced"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, RolloutEffect::UpdateCurrentWave { .. })),
            "HostJoined MUST NOT emit UpdateCurrentWave; emitting it would persist the wrong cursor in the rollouts table"
        );
        // The transition itself still fires.
        assert_eq!(record.state, RolloutState::Active);
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
