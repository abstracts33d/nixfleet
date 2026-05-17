//! Transitions from `Pending`. Only legal events: `LocalActivate` (agent
//! side, after long-poll receives Dispatch + cross-check vs manifest) and
//! `RemoteDispatchAck` (CP mirror, after agent posts to `/v1/agent/events`).
//! Both drive `Pending → Activating`.

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutPolicy;

use crate::effect::{Effect, OutboundAgentEvent};
use crate::error::TransitionError;
use crate::event::Event;
use crate::state::{HostRolloutState, HostState};

use super::illegal;

pub(super) fn handle(
    mut state: HostRolloutState,
    event: Event,
    _now: DateTime<Utc>,
    _policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    match event {
        Event::LocalActivate {
            current_closure_at_dispatch,
            // target_closure + soak_due_at are consumed by the agent
            // reducer bootstrap (sets state.target_closure /
            // state.soak_due_at before this handler runs); the
            // state-machine transition only flips state + stamps the
            // rollback target and dispatch_acked_at timestamps.
            target_closure: _,
            received_at,
            soak_due_at: _,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Activating;
            state.current_closure_at_dispatch = Some(current_closure_at_dispatch.clone());
            state.dispatch_acked_at = Some(received_at);
            state.last_event_seq = seq;

            let target = state.target_closure.clone();
            let rollout_id = state.rollout_id.clone();
            let effects = vec![
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: rollout_id.clone(),
                    from,
                    to: HostState::Activating,
                    at: received_at,
                },
                Effect::LocalEmitEvent {
                    rollout_id: rollout_id.clone(),
                    payload: OutboundAgentEvent::DispatchAck {
                        current_closure_at_dispatch,
                        received_at,
                        seq,
                    },
                    durable: true,
                },
                Effect::LocalFireSwitch { rollout_id, target },
            ];
            Ok((state, effects))
        }

        Event::RemoteDispatchAck {
            current_closure_at_dispatch,
            received_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Activating;
            state.current_closure_at_dispatch = Some(current_closure_at_dispatch);
            state.dispatch_acked_at = Some(received_at);
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::DispatchAck {
                        current_closure_at_dispatch: state
                            .current_closure_at_dispatch
                            .clone()
                            .unwrap_or_default(),
                        received_at,
                        seq,
                    },
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Activating,
                    at: received_at,
                },
            ];
            Ok((state, effects))
        }

        other => Err(illegal(&state, &other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transitions::dispatch;
    use chrono::{TimeZone, Utc};
    use nixfleet_proto::{HealthGate, OnHealthFailure, RolloutPolicy};

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn fake_policy() -> RolloutPolicy {
        RolloutPolicy {
            strategy: "all-at-once".into(),
            waves: vec![],
            health_gate: HealthGate::default(),
            on_health_failure: OnHealthFailure::Halt,
        }
    }

    fn pending() -> HostRolloutState {
        HostRolloutState::new_pending(
            "r1".into(),
            "h1".into(),
            "stable".into(),
            "abc123".into(),
            t0(),
            t0() + chrono::Duration::minutes(5),
        )
    }

    #[test]
    fn local_activate_moves_pending_to_activating() {
        let state = pending();
        let event = Event::LocalActivate {
            current_closure_at_dispatch: "prior-closure".into(),
            target_closure: "target".into(),
            received_at: t0() + chrono::Duration::seconds(1),
            soak_due_at: t0() + chrono::Duration::minutes(5),
            seq: 1,
        };
        let (next, effects) = dispatch(state, event, t0(), &fake_policy()).unwrap();
        assert_eq!(next.state, HostState::Activating);
        assert_eq!(
            next.current_closure_at_dispatch.as_deref(),
            Some("prior-closure")
        );
        assert!(next.dispatch_acked_at.is_some());
        assert_eq!(next.last_event_seq, 1);
        assert!(matches!(effects[0], Effect::RecordTransition { .. }));
        assert!(matches!(effects[1], Effect::LocalEmitEvent { .. }));
        assert!(matches!(effects[2], Effect::LocalFireSwitch { .. }));
    }

    #[test]
    fn remote_dispatch_ack_mirrors_pending_to_activating() {
        let state = pending();
        let event = Event::RemoteDispatchAck {
            current_closure_at_dispatch: "prior-closure".into(),
            received_at: t0() + chrono::Duration::seconds(1),
            seq: 1,
        };
        let (next, effects) = dispatch(state, event, t0(), &fake_policy()).unwrap();
        assert_eq!(next.state, HostState::Activating);
        assert!(matches!(effects[0], Effect::RemoteAppendEventLog { .. }));
        assert!(matches!(effects[1], Effect::RecordTransition { .. }));
    }

    #[test]
    fn illegal_event_in_pending_returns_illegal_for_state() {
        let state = pending();
        let event = Event::LocalActivationCompleted {
            observed_current_closure: "abc123".into(),
            exit_code: 0,
            completed_at: t0(),
            seq: 1,
        };
        let err = dispatch(state, event, t0(), &fake_policy()).unwrap_err();
        assert!(matches!(err, TransitionError::IllegalForState { .. }));
    }

    #[test]
    fn seq_regression_rejected() {
        let mut state = pending();
        state.last_event_seq = 5;
        let event = Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0(),
            soak_due_at: t0() + chrono::Duration::minutes(5),
            seq: 5, // <= last_event_seq
        };
        let err = dispatch(state, event, t0(), &fake_policy()).unwrap_err();
        assert!(matches!(err, TransitionError::SeqRegression { .. }));
    }
}
