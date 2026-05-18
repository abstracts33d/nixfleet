//! Cross-state proptest invariants for the rollout reducer
//! (RFC-0008 §10). Per-source-state unit tests live alongside each
//! `transitions/<state>.rs`; this module covers the global invariants
//! that hold across any legal event sequence.

#![cfg(test)]

use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;

use crate::HostState;
use crate::rollout::{
    RolloutEffect, RolloutEvent, RolloutRecord, RolloutState, RolloutTransitionError, step,
};

fn t0() -> DateTime<Utc> {
    use chrono::TimeZone;
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

/// Generator for any legal event variant with stable fixed metadata. We
/// don't try to generate event-rollout-id correspondence — the
/// reducer treats `rollout_id` as a label, not a constraint, in 10b.
fn arb_event(at: DateTime<Utc>) -> impl Strategy<Value = RolloutEvent> {
    prop_oneof![
        Just(RolloutEvent::RolloutOpened {
            rollout_id: "r1".into(),
            channel: "stable".into(),
            target_ref: "ref-1".into(),
            at,
        }),
        (0u32..4).prop_map(move |wave| RolloutEvent::HostJoined {
            rollout_id: "r1".into(),
            host_id: format!("h{wave}"),
            wave,
            at,
        }),
        prop_oneof![
            Just(HostState::Pending),
            Just(HostState::Activating),
            Just(HostState::Soaking),
            Just(HostState::Converged),
            Just(HostState::Failed),
            Just(HostState::Reverted),
        ]
        .prop_flat_map(move |from| {
            prop_oneof![
                Just(HostState::Pending),
                Just(HostState::Activating),
                Just(HostState::Soaking),
                Just(HostState::Converged),
                Just(HostState::Failed),
                Just(HostState::Reverted),
            ]
            .prop_map(move |to| RolloutEvent::HostStateChanged {
                rollout_id: "r1".into(),
                host_id: "h1".into(),
                from,
                to,
                at,
            })
        }),
        (0u32..4, 0u32..4).prop_map(move |(from_wave, to_wave)| RolloutEvent::WaveAdvanced {
            rollout_id: "r1".into(),
            from_wave,
            to_wave,
            at,
        }),
        Just(RolloutEvent::RolloutTerminal {
            rollout_id: "r1".into(),
            at,
        }),
        Just(RolloutEvent::SuccessorOpened {
            superseded_rollout_id: "r1".into(),
            successor_rollout_id: "r2".into(),
            at,
        }),
        Just(RolloutEvent::RetentionExpired {
            rollout_id: "r1".into(),
            at,
        }),
    ]
}

proptest! {
    /// **Invariant** (RFC-0008 §3): if a transition fires and the new
    /// state is `Terminal`, `terminal_at` is populated.
    #[test]
    fn terminal_implies_terminal_at(
        events in proptest::collection::vec(arb_event(t0()), 1..32)
    ) {
        let mut record = opening_record();
        let mut now = t0();
        for event in events {
            now += Duration::seconds(1);
            if let Ok((next, _effects)) = step(record.clone(), event, now) {
                if next.state == RolloutState::Terminal {
                    prop_assert!(
                        next.terminal_at.is_some(),
                        "Terminal state must carry terminal_at",
                    );
                }
                record = next;
            }
        }
    }

    /// **Invariant**: if a transition fires and the new state is
    /// `Superseded`, `superseded_at` is populated.
    #[test]
    fn superseded_implies_superseded_at(
        events in proptest::collection::vec(arb_event(t0()), 1..32)
    ) {
        let mut record = opening_record();
        let mut now = t0();
        for event in events {
            now += Duration::seconds(1);
            if let Ok((next, _effects)) = step(record.clone(), event, now) {
                if next.state == RolloutState::Superseded {
                    prop_assert!(
                        next.superseded_at.is_some(),
                        "Superseded state must carry superseded_at",
                    );
                }
                record = next;
            }
        }
    }

    /// **Invariant**: `Pruned` is absorbing — every subsequent event
    /// returns `Err(IllegalForState { from: Pruned, .. })`.
    #[test]
    fn pruned_is_absorbing(event in arb_event(t0())) {
        let pruned = RolloutRecord {
            state: RolloutState::Pruned,
            ..opening_record()
        };
        let result = step(pruned, event, t0());
        match result {
            Err(RolloutTransitionError::IllegalForState {
                from: RolloutState::Pruned,
                ..
            }) => {}
            other => prop_assert!(false, "Pruned must reject every event, got {other:?}"),
        }
    }

    /// **Invariant**: `current_wave` is monotonically non-decreasing
    /// across any legal event sequence (no transition rolls it back).
    #[test]
    fn current_wave_is_monotonic(
        events in proptest::collection::vec(arb_event(t0()), 1..32)
    ) {
        let mut record = opening_record();
        let mut last_wave = record.current_wave;
        let mut now = t0();
        for event in events {
            now += Duration::seconds(1);
            if let Ok((next, _effects)) = step(record.clone(), event, now) {
                prop_assert!(
                    next.current_wave >= last_wave,
                    "current_wave regressed from {last_wave} to {} on transition",
                    next.current_wave,
                );
                last_wave = next.current_wave;
                record = next;
            }
        }
    }

    /// **Effect contract**: every `RecordRolloutTransition` effect's
    /// `(from, to)` pair matches a real state change on the resulting
    /// record (no spurious transition events).
    #[test]
    fn record_rollout_transition_matches_actual_change(
        events in proptest::collection::vec(arb_event(t0()), 1..32)
    ) {
        let mut record = opening_record();
        let mut now = t0();
        for event in events {
            now += Duration::seconds(1);
            let pre_state = record.state;
            if let Ok((next, effects)) = step(record.clone(), event, now) {
                for effect in &effects {
                    if let RolloutEffect::RecordRolloutTransition { from, to, .. } = effect {
                        prop_assert_eq!(
                            *from, pre_state,
                            "transition effect's 'from' must match pre-step state",
                        );
                        prop_assert_eq!(
                            *to, next.state,
                            "transition effect's 'to' must match post-step state",
                        );
                    }
                }
                record = next;
            }
        }
    }
}
