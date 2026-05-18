//! Transitions from `Deferred`.
//!
//! Activation was set up at the bootloader level but the live switch
//! was deferred because a critical component (dbus/systemd/kernel/init)
//! cannot be live-swapped on a running system. The host stays Deferred
//! until the operator reboots; on reboot the agent's boot-recovery
//! handshake observes `current_closure == target_closure` and CP's
//! `handle_heartbeat` synthesis (LIFT #1) drives the transition out
//! via `RemoteActivationCompleted`.
//!
//! Legal events from Deferred:
//!   - `LocalActivationCompleted` / `RemoteActivationCompleted` —
//!     post-reboot, the activation has actually taken; drives
//!     `Deferred → Soaking` (same target state as Activating →
//!     Soaking). LIFT #1's CP-side synthesis is the typical emitter.
//!   - `LocalActivationFailed` / `RemoteActivationFailed` — operator
//!     rebooted but boot failed / verification failed. Drives
//!     `Deferred → Failed`. Rare; covered for completeness.
//!
//! All other events are illegal from Deferred (probe events: probes
//! aren't running yet because activation hasn't completed; rollback
//! events: nothing to roll back from at the in-memory state level —
//! the bootloader has the new target; rollback semantics for Deferred
//! is a v0.3 design question).

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
        // Deferred → Soaking. Agent rebooted; activation took live;
        // current_closure now matches target. Typical emitter is
        // LIFT #1's CP-side synthesis (handle_heartbeat sees
        // current == target while state ∈ {Activating, Deferred}).
        Event::LocalActivationCompleted {
            observed_current_closure,
            exit_code,
            completed_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Soaking;
            state.activation_completed_at = Some(completed_at);
            state.current_closure = Some(observed_current_closure.clone());
            state.probes.clear();
            state.last_event_seq = seq;

            let effects = vec![
                Effect::LocalResetProbeCache {
                    rollout_id: state.rollout_id.clone(),
                },
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationCompleted {
                        observed_current_closure,
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
                    to: HostState::Soaking,
                    at: completed_at,
                },
            ];
            Ok((state, effects))
        }
        Event::RemoteActivationCompleted {
            observed_current_closure,
            exit_code,
            completed_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Soaking;
            state.activation_completed_at = Some(completed_at);
            state.current_closure = Some(observed_current_closure.clone());
            state.probes.clear();
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationCompleted {
                        observed_current_closure,
                        exit_code,
                        completed_at,
                        seq,
                    },
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Soaking,
                    at: completed_at,
                },
            ];
            Ok((state, effects))
        }

        // Deferred → Failed. Operator rebooted but boot failed or
        // verification failed. Same shape as Activating → Failed but
        // from the Deferred source.
        Event::LocalActivationFailed {
            exit_code,
            stderr_tail,
            failed_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Failed;
            state.activation_failed_at = Some(failed_at);
            state.failed_at = Some(failed_at);
            state.last_event_seq = seq;

            let effects = vec![
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationFailed {
                        exit_code,
                        stderr_tail,
                        failed_at,
                        seq,
                    },
                    durable: true,
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Failed,
                    at: failed_at,
                },
            ];
            Ok((state, effects))
        }
        Event::RemoteActivationFailed {
            exit_code,
            stderr_tail,
            failed_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Failed;
            state.activation_failed_at = Some(failed_at);
            state.failed_at = Some(failed_at);
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationFailed {
                        exit_code,
                        stderr_tail,
                        failed_at,
                        seq,
                    },
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Failed,
                    at: failed_at,
                },
            ];
            Ok((state, effects))
        }

        _ => Err(illegal(&state, &event)),
    }
}
