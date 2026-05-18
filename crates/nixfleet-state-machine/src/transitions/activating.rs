//! Transitions from `Activating`.
//!
//! - `LocalActivationStarted` / `RemoteActivationStarted` — visibility, no
//!   state change; stamps `activation_started_at`.
//! - `LocalActivationCompleted` / `RemoteActivationCompleted` — drives
//!   `Activating → Soaking`, sets `current_closure`, resets probe cache.
//! - `LocalActivationFailed` / `RemoteActivationFailed` — drives
//!   `Activating → Failed`; agent reads `onHealthFailure` from policy and
//!   (if `rollback-and-halt`) fires the rollback in the same handler.

use chrono::{DateTime, Utc};
use nixfleet_proto::{OnHealthFailure, RolloutPolicy};

use crate::effect::{Effect, OutboundAgentEvent};
use crate::error::TransitionError;
use crate::event::Event;
use crate::state::{HostRolloutState, HostState};

use super::illegal;

pub(super) fn handle(
    mut state: HostRolloutState,
    event: Event,
    _now: DateTime<Utc>,
    policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    match event {
        // Visibility — no transition, stamp + emit
        Event::LocalActivationStarted {
            started_at,
            switch_method,
            seq,
        } => {
            state.activation_started_at = Some(started_at);
            state.last_event_seq = seq;
            let effects = vec![Effect::LocalEmitEvent {
                rollout_id: state.rollout_id.clone(),
                payload: OutboundAgentEvent::ActivationStarted {
                    started_at,
                    switch_method,
                    seq,
                },
                durable: true,
            }];
            Ok((state, effects))
        }
        Event::RemoteActivationStarted {
            started_at,
            switch_method,
            seq,
        } => {
            state.activation_started_at = Some(started_at);
            state.last_event_seq = seq;
            let effects = vec![Effect::RemoteAppendEventLog {
                host: state.hostname.clone(),
                rollout_id: state.rollout_id.clone(),
                payload: OutboundAgentEvent::ActivationStarted {
                    started_at,
                    switch_method,
                    seq,
                },
            }];
            Ok((state, effects))
        }

        // Activating → Soaking
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

        // Activating → Activating (no state change). Live switch was
        // deferred because a critical component (dbus/systemd/kernel)
        // cannot be live-swapped. Profile + bootloader are correct;
        // the activation completes on next reboot via boot-recovery
        // (LIFT #1's handle_heartbeat synthesis).
        // Activating → Deferred. Live switch skipped because a critical
        // component (dbus/systemd/kernel/init) cannot be live-swapped.
        // The host is ordering-eligible (cascade can progress past it
        // for host-edges + wave-promotion + advance_current_waves) but
        // not health-verified (channel-edges still waits for actual
        // Converged). On reboot, the agent's boot-recovery handshake
        // triggers LIFT #1's `RemoteActivationCompleted` synthesis →
        // `Deferred → Soaking` via deferred.rs.
        Event::LocalActivationDeferred {
            component,
            deferred_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Deferred;
            state.last_event_seq = seq;
            let effects = vec![
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationDeferred {
                        component,
                        deferred_at,
                        seq,
                    },
                    durable: true,
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Deferred,
                    at: deferred_at,
                },
            ];
            Ok((state, effects))
        }

        // CP-side mirror of LocalActivationDeferred. Same transition
        // shape (Activating → Deferred), but uses RemoteAppendEventLog
        // (CP writes directly, bypassing the outbound queue).
        Event::RemoteActivationDeferred {
            component,
            deferred_at,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Deferred;
            state.last_event_seq = seq;
            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::ActivationDeferred {
                        component,
                        deferred_at,
                        seq,
                    },
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Deferred,
                    at: deferred_at,
                },
            ];
            Ok((state, effects))
        }

        // Activating → Failed
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
            state.policy_applied = Some(policy.on_health_failure);
            state.last_event_seq = seq;

            let mut effects = vec![
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
            // RFC-0008 §4.1 + §4.2: rollback is agent-decided from manifest
            // policy. No CP signal — agent fires the switch immediately.
            if matches!(policy.on_health_failure, OnHealthFailure::RollbackAndHalt)
                && let Some(prior) = state.current_closure_at_dispatch.clone()
            {
                effects.push(Effect::LocalFireRollbackTo {
                    rollout_id: state.rollout_id.clone(),
                    closure: prior,
                });
            }
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
            state.policy_applied = Some(policy.on_health_failure);
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

        other => Err(illegal(&state, &other)),
    }
}
