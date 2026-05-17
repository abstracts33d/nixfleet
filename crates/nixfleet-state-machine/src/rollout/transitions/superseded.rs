//! `Superseded` source state. A newer rollout for the same channel has
//! opened. Only exit is `→ Pruned` on `RetentionExpired`.

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
        RolloutEvent::RetentionExpired { at, .. } => {
            record.state = RolloutState::Pruned;
            let effects = vec![transition_effect(
                &record,
                RolloutState::Superseded,
                RolloutState::Pruned,
                at,
            )];
            Ok((record, effects))
        }
        // Late host events and aggregations from a superseded rollout
        // are noise (predecessor was already replaced).
        RolloutEvent::HostStateChanged { .. }
        | RolloutEvent::HostJoined { .. }
        | RolloutEvent::WaveAdvanced { .. }
        | RolloutEvent::RolloutTerminal { .. }
        // Re-emitted SuccessorOpened (multiple successors landed) is
        // idempotent — we're already Superseded.
        | RolloutEvent::SuccessorOpened { .. } => Ok((record, Vec::new())),
        RolloutEvent::RolloutOpened { .. } | RolloutEvent::OperatorClearance { .. } => Err(
            illegal(RolloutState::Superseded, &event, record.rollout_id.clone()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn superseded_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Superseded,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: None,
            superseded_at: Some(t0()),
        }
    }

    #[test]
    fn retention_expired_transitions_to_pruned() {
        let event = RolloutEvent::RetentionExpired {
            rollout_id: "r1".into(),
            at: t0(),
        };
        let (record, _) = step(superseded_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Pruned);
    }

    #[test]
    fn re_successor_opened_is_idempotent() {
        let event = RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r3".into(),
            at: t0(),
        };
        let (record, effects) = step(superseded_record(), event, t0()).unwrap();
        assert_eq!(record.state, RolloutState::Superseded);
        assert!(effects.is_empty());
    }
}
