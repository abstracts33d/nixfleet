//! Transitions from `Failed`. Legal events:
//!
//! - `LocalRollbackCompleted` — agent has executed rollback per manifest
//!   policy; drives `Failed → Reverted`. Single signed source of truth is
//!   the manifest; CP issued no signal (RFC-0005 §4.1).
//! - `RemoteRollbackComplete` — CP mirror sees the same; emits
//!   `RemoteInsertQuarantine` for the bad closure on the channel.
//!
//! For `halt-only` policy, `Failed` is terminal — no rollback event
//! arrives, operator action lifts it via a new signed manifest.

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
        Event::LocalRollbackCompleted {
            reverted_to_closure,
            exit_code,
            completed_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Reverted;
            state.reverted_at = Some(completed_at);
            state.reverted_to = Some(reverted_to_closure.clone());
            state.current_closure = Some(reverted_to_closure.clone());
            state.last_event_seq = seq;

            let effects = vec![
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::RollbackComplete {
                        reverted_to_closure,
                        exit_code,
                        completed_at,
                        seq,
                    },
                    durable: true,
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Reverted,
                    at: completed_at,
                },
            ];
            Ok((state, effects))
        }
        Event::RemoteRollbackComplete {
            reverted_to_closure,
            exit_code,
            completed_at,
            seq,
        } => {
            let from = state.state;
            let bad_closure = state.target_closure.clone();
            let channel = state.channel.clone();
            state.state = HostState::Reverted;
            state.reverted_at = Some(completed_at);
            state.reverted_to = Some(reverted_to_closure.clone());
            state.current_closure = Some(reverted_to_closure.clone());
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::RollbackComplete {
                        reverted_to_closure,
                        exit_code,
                        completed_at,
                        seq,
                    },
                },
                Effect::RemoteInsertQuarantine {
                    channel,
                    closure: bad_closure,
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Reverted,
                    at: completed_at,
                },
            ];
            Ok((state, effects))
        }

        other => Err(illegal(&state, &other)),
    }
}
