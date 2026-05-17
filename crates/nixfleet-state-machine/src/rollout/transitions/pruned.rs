//! `Pruned` source state. Absorbing — the in-memory state-machine
//! instance has been freed; the row persists for audit (RFC-0012 §3 +
//! d1bc6df1 architect fix-up). Any further event is structurally a
//! defect.

use chrono::{DateTime, Utc};

use super::illegal;
use crate::rollout::effect::RolloutEffect;
use crate::rollout::error::RolloutTransitionError;
use crate::rollout::event::RolloutEvent;
use crate::rollout::state::{RolloutRecord, RolloutState};

pub(super) fn step(
    record: RolloutRecord,
    event: RolloutEvent,
    _now: DateTime<Utc>,
) -> Result<(RolloutRecord, Vec<RolloutEffect>), RolloutTransitionError> {
    Err(illegal(RolloutState::Pruned, &event, record.rollout_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn pruned_record() -> RolloutRecord {
        RolloutRecord {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            state: RolloutState::Pruned,
            current_wave: 0,
            opened_event_log_seq: None,
            last_transition_event_log_seq: None,
            opened_at: t0(),
            terminal_at: Some(t0()),
            superseded_at: None,
        }
    }

    #[test]
    fn every_event_is_illegal_from_pruned() {
        let event = RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r2".into(),
            at: t0(),
        };
        let err = step(pruned_record(), event, t0()).unwrap_err();
        assert!(matches!(
            err,
            RolloutTransitionError::IllegalForState {
                from: RolloutState::Pruned,
                ..
            }
        ));
    }
}
