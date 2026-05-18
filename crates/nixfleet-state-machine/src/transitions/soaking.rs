//! Transitions from `Soaking`.
//!
//! - `LocalProbeObservedFirst` / `RemoteProbeObservedFirst` — stamps
//!   `probe_observed_first_at`. Soak gate may now consult probe state.
//! - `LocalProbeResult` / `RemoteProbeResult` — updates probe map.
//! - `LocalProbeFailureFirst` / `RemoteProbeFailureFirst` — stamps
//!   `probe_failure_first_at`. Sweep window measured from this exact
//!   agent-supplied timestamp.
//! - `LocalSustainedFailureCrossed` / `RemoteFailed` — drives
//!   `Soaking → Failed`. Agent reads `onHealthFailure` and (if
//!   `rollback-and-halt`) fires rollback in same handler.
//! - `LocalConvergedReached` / `RemoteConverged` — drives
//!   `Soaking → Converged` after re-verifying the three §4.2 invariants.

use chrono::{DateTime, Utc};
use nixfleet_proto::{OnHealthFailure, RolloutPolicy};

use crate::effect::{Effect, OutboundAgentEvent};
use crate::error::TransitionError;
use crate::event::Event;
use crate::state::{HostRolloutState, HostState, ProbeMode, ProbeRecord, ProbeStatus};

use super::illegal;

pub(super) fn handle(
    mut state: HostRolloutState,
    event: Event,
    _now: DateTime<Utc>,
    policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    match event {
        Event::LocalProbeTopologyDeclared {
            probes,
            declared_at,
            seq,
        } => {
            state.last_event_seq = seq;
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::LocalEmitEvent {
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeTopologyDeclared {
                        probes,
                        declared_at,
                        seq,
                    },
                    durable: true,
                }],
            ))
        }
        Event::RemoteProbeTopologyDeclared {
            probes,
            declared_at,
            seq,
        } => {
            state.last_event_seq = seq;
            let host = state.hostname.clone();
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::RemoteAppendEventLog {
                    host,
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeTopologyDeclared {
                        probes,
                        declared_at,
                        seq,
                    },
                }],
            ))
        }

        Event::LocalProbeObservedFirst {
            probe_name,
            mode,
            observed_at,
            seq,
        } => {
            if state.probe_observed_first_at.is_none() {
                state.probe_observed_first_at = Some(observed_at);
            }
            state.last_event_seq = seq;
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::LocalEmitEvent {
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeObservedFirst {
                        probe_name,
                        mode,
                        observed_at,
                        seq,
                    },
                    durable: false,
                }],
            ))
        }
        Event::RemoteProbeObservedFirst {
            probe_name,
            mode,
            observed_at,
            seq,
        } => {
            if state.probe_observed_first_at.is_none() {
                state.probe_observed_first_at = Some(observed_at);
            }
            state.last_event_seq = seq;
            Ok((
                state.clone(),
                vec![Effect::RemoteAppendEventLog {
                    host: state.hostname,
                    rollout_id: state.rollout_id,
                    payload: OutboundAgentEvent::ProbeObservedFirst {
                        probe_name,
                        mode,
                        observed_at,
                        seq,
                    },
                }],
            ))
        }

        Event::LocalProbeResult {
            probe_name,
            mode,
            status,
            observed_at,
            failure_reason,
            sub_results,
            seq,
        } => {
            update_probe(
                &mut state,
                &probe_name,
                mode,
                status,
                observed_at,
                failure_reason.clone(),
            );
            state.last_event_seq = seq;
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::LocalEmitEvent {
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeResult {
                        probe_name,
                        mode,
                        status,
                        observed_at,
                        failure_reason,
                        sub_results,
                        seq,
                    },
                    durable: false,
                }],
            ))
        }
        Event::RemoteProbeResult {
            probe_name,
            mode,
            status,
            observed_at,
            failure_reason,
            sub_results,
            seq,
        } => {
            update_probe(
                &mut state,
                &probe_name,
                mode,
                status,
                observed_at,
                failure_reason.clone(),
            );
            state.last_event_seq = seq;
            let host = state.hostname.clone();
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::RemoteAppendEventLog {
                    host,
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeResult {
                        probe_name,
                        mode,
                        status,
                        observed_at,
                        failure_reason,
                        sub_results,
                        seq,
                    },
                }],
            ))
        }

        Event::LocalProbeFailureFirst {
            probe_name,
            mode,
            first_failed_at,
            seq,
        } => {
            if state.probe_failure_first_at.is_none() {
                state.probe_failure_first_at = Some(first_failed_at);
            }
            state.last_event_seq = seq;
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::LocalEmitEvent {
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeFailureFirst {
                        probe_name,
                        mode,
                        first_failed_at,
                        seq,
                    },
                    durable: false,
                }],
            ))
        }
        Event::RemoteProbeFailureFirst {
            probe_name,
            mode,
            first_failed_at,
            seq,
        } => {
            if state.probe_failure_first_at.is_none() {
                state.probe_failure_first_at = Some(first_failed_at);
            }
            state.last_event_seq = seq;
            let host = state.hostname.clone();
            let rollout_id = state.rollout_id.clone();
            Ok((
                state,
                vec![Effect::RemoteAppendEventLog {
                    host,
                    rollout_id,
                    payload: OutboundAgentEvent::ProbeFailureFirst {
                        probe_name,
                        mode,
                        first_failed_at,
                        seq,
                    },
                }],
            ))
        }

        // Soaking → Failed (agent self-detected sustained failure)
        Event::LocalSustainedFailureCrossed {
            failed_at,
            sustained_duration_secs,
            failing_probes,
            policy_applied,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Failed;
            state.failed_at = Some(failed_at);
            state.policy_applied = Some(policy_applied);
            state.last_event_seq = seq;

            let mut effects = vec![
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::Failed {
                        failed_at,
                        sustained_duration_secs,
                        failing_probes: failing_probes.clone(),
                        policy_applied,
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
            if matches!(policy_applied, OnHealthFailure::RollbackAndHalt)
                && let Some(prior) = state.current_closure_at_dispatch.clone()
            {
                effects.push(Effect::LocalFireRollbackTo {
                    rollout_id: state.rollout_id.clone(),
                    closure: prior,
                });
            }
            Ok((state, effects))
        }
        Event::RemoteFailed {
            failed_at,
            sustained_duration_secs,
            failing_probes,
            policy_applied,
            seq,
        } => {
            let from = state.state;
            state.state = HostState::Failed;
            state.failed_at = Some(failed_at);
            state.policy_applied = Some(policy_applied);
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::Failed {
                        failed_at,
                        sustained_duration_secs,
                        failing_probes,
                        policy_applied,
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

        // Soaking → Converged (after re-verifying RFC-0005 §4.2 invariants)
        Event::LocalConvergedReached {
            converged_at,
            current_closure,
            seq,
        } => {
            verify_converged_invariants(&state, &current_closure, converged_at)?;
            let from = state.state;
            state.state = HostState::Converged;
            state.converged_at = Some(converged_at);
            state.current_closure = Some(current_closure.clone());
            state.last_event_seq = seq;

            let effects = vec![
                Effect::LocalEmitEvent {
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::Converged {
                        converged_at,
                        current_closure,
                        seq,
                    },
                    durable: true,
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Converged,
                    at: converged_at,
                },
            ];
            // policy currently unused for Converged but available for future
            // wave-promotion side effects.
            let _ = policy;
            Ok((state, effects))
        }
        Event::RemoteConverged {
            converged_at,
            current_closure,
            seq,
        } => {
            verify_converged_invariants(&state, &current_closure, converged_at)?;
            let from = state.state;
            state.state = HostState::Converged;
            state.converged_at = Some(converged_at);
            state.current_closure = Some(current_closure.clone());
            state.last_event_seq = seq;

            let effects = vec![
                Effect::RemoteAppendEventLog {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    payload: OutboundAgentEvent::Converged {
                        converged_at,
                        current_closure,
                        seq,
                    },
                },
                Effect::RecordTransition {
                    host: state.hostname.clone(),
                    rollout_id: state.rollout_id.clone(),
                    from,
                    to: HostState::Converged,
                    at: converged_at,
                },
            ];
            Ok((state, effects))
        }

        other => Err(illegal(&state, &other)),
    }
}

fn update_probe(
    state: &mut HostRolloutState,
    name: &str,
    mode: crate::state::ProbeMode,
    status: ProbeStatus,
    observed_at: chrono::DateTime<chrono::Utc>,
    failure_reason: Option<String>,
) {
    let entry = state.probes.entry(name.to_string()).or_insert(ProbeRecord {
        status,
        mode,
        last_observed_at: observed_at,
        last_pass_at: None,
        failure_reason: None,
    });
    entry.status = status;
    entry.mode = mode;
    entry.last_observed_at = observed_at;
    if matches!(status, ProbeStatus::Pass) {
        entry.last_pass_at = Some(observed_at);
        entry.failure_reason = None;
    } else {
        entry.failure_reason = failure_reason;
    }
}

/// RFC-0005 §4.2 `Converged` event: CP re-verifies the three invariants
/// before transitioning. Same check runs agent-side too — same code, both
/// sides (RFC-0006 §2 principle 4).
fn verify_converged_invariants(
    state: &HostRolloutState,
    current_closure: &str,
    converged_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), TransitionError> {
    // Invariant 1: current_closure == target_closure
    if current_closure != state.target_closure {
        return Err(TransitionError::Invariant(
            "Converged event: current_closure != target_closure",
        ));
    }
    // Invariant 2: soak_due_at has elapsed
    if let Some(soak_due) = state.soak_due_at
        && converged_at < soak_due
    {
        return Err(TransitionError::Invariant(
            "Converged event: converged_at before soak_due_at",
        ));
    }
    // Invariant 3: all enforce-mode declared probes are Pass. Observe and
    // Disabled probes do not gate per RFC-0007 §3.3 (ProbeMode docstring,
    // state.rs); the wave-promotion gate already filters by mode, and the
    // sustained-failure path (reducer's collect_failing_enforce_probes)
    // is on the same side of the line — this brings convergence into
    // parity. An empty enforce set is acceptable (no enforce probes
    // declared = no probe constraint).
    for probe in state.probes.values() {
        if matches!(probe.mode, ProbeMode::Enforce) && !matches!(probe.status, ProbeStatus::Pass) {
            return Err(TransitionError::Invariant(
                "Converged event: at least one enforce-mode probe is not Pass",
            ));
        }
    }
    Ok(())
}
