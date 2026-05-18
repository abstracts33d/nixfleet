//! `Active` source state. At least one host is in-flight
//! (`Pending`/`Activating`/`Soaking` per RFC-0005).

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
        // Hosts can keep joining as later waves dispatch — stays Active.
        //
        // LOADBEARING: HostJoined is observed (the wave is logged in
        // event_log via the event itself) but does NOT mutate
        // `current_wave`. Wave-cursor progression happens via
        // `advance_current_waves` → `WaveAdvanced`. See
        // opening.rs::step's HostJoined arm for the full rationale.
        RolloutEvent::HostJoined { .. } => Ok((record, Vec::new())),

        // Per-host transitions drive aggregate state changes.
        RolloutEvent::HostStateChanged { to, at, .. } => match to {
            HostState::Reverted => {
                record.state = RolloutState::Reverted;
                let effects = vec![transition_effect(
                    &record,
                    RolloutState::Active,
                    RolloutState::Reverted,
                    at,
                )];
                Ok((record, effects))
            }
            HostState::Failed => {
                record.state = RolloutState::Failed;
                let effects = vec![transition_effect(
                    &record,
                    RolloutState::Active,
                    RolloutState::Failed,
                    at,
                )];
                Ok((record, effects))
            }
            // All other host transitions (→ Activating, → Soaking,
            // → Converged) are observed but don't change the rollout
            // state directly; the applier emits `RolloutTerminal` or
            // `WaveAdvanced` separately when aggregates flip.
            _ => Ok((record, Vec::new())),
        },

        // Wave aggregation. The applier emits this when the current
        // wave's hosts have all reached Soaking (or beyond). If
        // `to_wave > from_wave` the planner has dispatched the next
        // wave → stays Active; if `to_wave == from_wave` the wave is
        // complete and no successor is dispatching → Converging.
        RolloutEvent::WaveAdvanced {
            from_wave,
            to_wave,
            at,
            ..
        } => {
            if to_wave > from_wave {
                // Monotonic — never roll back. A stale `WaveAdvanced`
                // for an earlier wave is observed but doesn't update.
                let mut effects = Vec::new();
                if to_wave > record.current_wave {
                    effects.push(RolloutEffect::UpdateCurrentWave {
                        rollout_id: record.rollout_id.clone(),
                        wave: to_wave,
                    });
                    record.current_wave = to_wave;
                }
                Ok((record, effects))
            } else {
                record.state = RolloutState::Converging;
                let effects = vec![transition_effect(
                    &record,
                    RolloutState::Active,
                    RolloutState::Converging,
                    at,
                )];
                Ok((record, effects))
            }
        }

        // All hosts in all waves Converged — applier aggregated.
        RolloutEvent::RolloutTerminal { at, .. } => {
            record.state = RolloutState::Terminal;
            record.terminal_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Active,
                RolloutState::Terminal,
                at,
            )];
            Ok((record, effects))
        }

        RolloutEvent::SuccessorOpened { at, .. } => {
            record.state = RolloutState::Superseded;
            record.superseded_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Active,
                RolloutState::Superseded,
                at,
            )];
            Ok((record, effects))
        }

        RolloutEvent::RolloutOpened { .. }
        | RolloutEvent::RetentionExpired { .. }
        | RolloutEvent::OperatorClearance { .. } => Err(illegal(
            RolloutState::Active,
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

    fn active_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Active,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: None,
            superseded_at: None,
        }
    }

    #[test]
    fn host_state_changed_to_reverted_transitions() {
        let event = RolloutEvent::HostStateChanged {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            from: HostState::Soaking,
            to: HostState::Reverted,
            at: t0(),
        };
        let (record, _) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Reverted);
    }

    #[test]
    fn host_state_changed_to_failed_transitions() {
        let event = RolloutEvent::HostStateChanged {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            from: HostState::Activating,
            to: HostState::Failed,
            at: t0(),
        };
        let (record, _) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Failed);
    }

    #[test]
    fn host_state_changed_to_soaking_is_observed_only() {
        let event = RolloutEvent::HostStateChanged {
            rollout_id: "r1".into(),
            host_id: "h1".into(),
            from: HostState::Activating,
            to: HostState::Soaking,
            at: t0(),
        };
        let (record, effects) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Active);
        assert!(effects.is_empty());
    }

    #[test]
    fn wave_advanced_to_higher_wave_stays_active() {
        let event = RolloutEvent::WaveAdvanced {
            rollout_id: "r1".into(),
            from_wave: 0,
            to_wave: 1,
            at: t0(),
        };
        let (record, effects) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Active);
        assert_eq!(record.current_wave, 1);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, RolloutEffect::UpdateCurrentWave { wave: 1, .. }))
        );
    }

    #[test]
    fn wave_advanced_to_same_wave_transitions_to_converging() {
        let event = RolloutEvent::WaveAdvanced {
            rollout_id: "r1".into(),
            from_wave: 0,
            to_wave: 0,
            at: t0(),
        };
        let (record, _) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Converging);
    }

    #[test]
    fn rollout_terminal_transitions_with_terminal_at() {
        let event = RolloutEvent::RolloutTerminal {
            rollout_id: "r1".into(),
            at: t0(),
        };
        let (record, _) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Terminal);
        assert_eq!(record.terminal_at, Some(t0()));
    }

    #[test]
    fn successor_opened_transitions_with_superseded_at() {
        let event = RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r2".into(),
            at: t0(),
        };
        let (record, _) = step(active_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Superseded);
        assert_eq!(record.superseded_at, Some(t0()));
    }

    #[test]
    fn rollout_opened_from_active_is_illegal() {
        let event = RolloutEvent::RolloutOpened {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            at: t0(),
        };
        let err = step(active_record(), event, t0()).unwrap_err();
        assert!(matches!(
            err,
            RolloutTransitionError::IllegalForState { .. }
        ));
    }
}
