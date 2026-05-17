//! `Terminal` source state. All hosts Converged; channel-edges may
//! release. Two exits: `→ Superseded` on `SuccessorOpened`,
//! `→ Pruned` on `RetentionExpired`.

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
        RolloutEvent::SuccessorOpened { at, .. } => {
            record.state = RolloutState::Superseded;
            record.superseded_at = Some(at);
            let effects = vec![transition_effect(
                &record,
                RolloutState::Terminal,
                RolloutState::Superseded,
                at,
            )];
            Ok((record, effects))
        }
        RolloutEvent::RetentionExpired { at, .. } => {
            record.state = RolloutState::Pruned;
            let effects = vec![transition_effect(
                &record,
                RolloutState::Terminal,
                RolloutState::Pruned,
                at,
            )];
            Ok((record, effects))
        }
        // Re-emitted terminal aggregation is a no-op (idempotency).
        RolloutEvent::RolloutTerminal { .. } => Ok((record, Vec::new())),
        // Host state changes after Terminal are stale ordering noise.
        RolloutEvent::HostStateChanged { .. } | RolloutEvent::HostJoined { .. } => {
            Ok((record, Vec::new()))
        }
        RolloutEvent::RolloutOpened { .. }
        | RolloutEvent::WaveAdvanced { .. }
        | RolloutEvent::OperatorClearance { .. } => Err(illegal(
            RolloutState::Terminal,
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

    fn terminal_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Terminal,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: Some(t0()),
            superseded_at: None,
        }
    }

    #[test]
    fn successor_opened_transitions_to_superseded() {
        let event = RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r2".into(),
            at: t0() + chrono::Duration::hours(1),
        };
        let (record, _) = step(terminal_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Superseded);
        assert!(record.superseded_at.is_some());
    }

    #[test]
    fn retention_expired_transitions_to_pruned() {
        let event = RolloutEvent::RetentionExpired {
            rollout_id: "r1".into(),
            at: t0() + chrono::Duration::hours(72),
        };
        let (record, _) = step(terminal_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Pruned);
    }

    #[test]
    fn idempotent_rollout_terminal_is_noop() {
        let event = RolloutEvent::RolloutTerminal {
            rollout_id: "r1".into(),
            at: t0(),
        };
        let (record, effects) = step(terminal_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Terminal);
        assert!(effects.is_empty());
    }
}
