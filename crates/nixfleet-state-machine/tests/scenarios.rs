//! Happy-path + critical-failure scenarios for the per-host reducer.
//! Property tests in `invariants.rs` cover the broader space via proptest.

use chrono::{DateTime, Duration, TimeZone, Utc};
use nixfleet_proto::{HealthGate, OnHealthFailure, RolloutPolicy};
use nixfleet_state_machine::{
    Effect, Event, HostRolloutState, HostState, OutboundAgentEvent, ProbeMode, ProbeStatus,
    ProbeSubResult, TransitionError, step,
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
}

fn policy_halt() -> RolloutPolicy {
    RolloutPolicy {
        strategy: "all-at-once".into(),
        waves: vec![],
        health_gate: HealthGate::default(),
        on_health_failure: OnHealthFailure::Halt,
    }
}

fn policy_rollback() -> RolloutPolicy {
    RolloutPolicy {
        strategy: "all-at-once".into(),
        waves: vec![],
        health_gate: HealthGate::default(),
        on_health_failure: OnHealthFailure::RollbackAndHalt,
    }
}

fn pending() -> HostRolloutState {
    HostRolloutState::new_pending(
        "r1".into(),
        "web-01".into(),
        "stable".into(),
        "target-closure-abc".into(),
        t0(),
        t0() + Duration::minutes(5),
    )
}

/// Full happy path: Pending → Activating → Soaking → Converged.
#[test]
fn happy_path_pending_to_converged() {
    let p = policy_halt();
    let s = pending();
    assert_eq!(s.state, HostState::Pending);

    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Activating);

    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Soaking);
    assert_eq!(s.current_closure.as_deref(), Some("target-closure-abc"));

    let (s, effects) = step(
        s,
        Event::LocalConvergedReached {
            converged_at: t0() + Duration::minutes(6),
            current_closure: "target-closure-abc".into(),
            seq: 3,
        },
        t0() + Duration::minutes(6),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Converged);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::RecordTransition {
            to: HostState::Converged,
            ..
        }
    )));
}

/// Sustained failure under `rollback-and-halt`: Soaking → Failed → Reverted.
/// The Failed transition emits a LocalFireRollbackTo effect; the
/// LocalRollbackCompleted event then drives Failed → Reverted.
#[test]
fn sustained_failure_rollback_path() {
    let p = policy_rollback();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior-closure".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Soaking);

    let (s, effects) = step(
        s,
        Event::LocalSustainedFailureCrossed {
            failed_at: t0() + Duration::seconds(125),
            sustained_duration_secs: 60,
            failing_probes: vec!["nginx-version".into()],
            policy_applied: OnHealthFailure::RollbackAndHalt,
            seq: 3,
        },
        t0() + Duration::seconds(125),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Failed);
    assert_eq!(s.policy_applied, Some(OnHealthFailure::RollbackAndHalt));
    let rollback = effects.iter().find_map(|e| match e {
        Effect::LocalFireRollbackTo { closure, .. } => Some(closure.clone()),
        _ => None,
    });
    assert_eq!(rollback.as_deref(), Some("prior-closure"));

    let (s, _) = step(
        s,
        Event::LocalRollbackCompleted {
            reverted_to_closure: "prior-closure".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(135),
            seq: 4,
        },
        t0() + Duration::seconds(135),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Reverted);
    assert_eq!(s.reverted_to.as_deref(), Some("prior-closure"));
}

/// `halt-only` policy: Soaking → Failed is terminal (no rollback fired).
#[test]
fn sustained_failure_halt_only_does_not_fire_rollback() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    let (s, effects) = step(
        s,
        Event::LocalSustainedFailureCrossed {
            failed_at: t0() + Duration::seconds(125),
            sustained_duration_secs: 60,
            failing_probes: vec!["nginx-version".into()],
            policy_applied: OnHealthFailure::Halt,
            seq: 3,
        },
        t0() + Duration::seconds(125),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Failed);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::LocalFireRollbackTo { .. })),
        "halt-only must not emit LocalFireRollbackTo"
    );
}

/// `Converged` event with `current != target` is rejected as
/// `Invariant` per RFC-0008 §4.2.
#[test]
fn converged_invariant_violation_rejected() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    let err = step(
        s,
        Event::LocalConvergedReached {
            converged_at: t0() + Duration::minutes(6),
            current_closure: "wrong-closure".into(), // mismatch with target
            seq: 3,
        },
        t0() + Duration::minutes(6),
        &p,
    )
    .unwrap_err();
    assert!(matches!(err, TransitionError::Invariant(_)));
}

/// Activation failure under rollback-and-halt fires LocalFireRollbackTo
/// without waiting for sustained-failure detection.
#[test]
fn activation_failure_rollback_and_halt_fires_rollback_immediately() {
    let p = policy_rollback();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior-closure".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, effects) = step(
        s,
        Event::LocalActivationFailed {
            exit_code: 1,
            stderr_tail: "switch failed".into(),
            failed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Failed);
    let rollback = effects.iter().find_map(|e| match e {
        Effect::LocalFireRollbackTo { closure, .. } => Some(closure.clone()),
        _ => None,
    });
    assert_eq!(rollback.as_deref(), Some("prior-closure"));
}

/// Probe results update the probe map and emit ProbeResult outbound events,
/// no state transition.
#[test]
fn probe_result_updates_map_without_state_change() {
    use nixfleet_state_machine::HostState;
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();
    assert!(
        s.probes.is_empty(),
        "ActivationCompleted resets probe cache"
    );

    let (s, effects) = step(
        s,
        Event::LocalProbeResult {
            probe_name: "nginx".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            status: ProbeStatus::Pass,
            observed_at: t0() + Duration::seconds(10),
            failure_reason: None,
            sub_results: None,
            seq: 3,
        },
        t0() + Duration::seconds(10),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Soaking);
    assert_eq!(s.probes.len(), 1);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::LocalEmitEvent {
            payload: OutboundAgentEvent::ProbeResult { .. },
            ..
        }
    )));
}

/// LIFT #2 + Option C: LocalActivationDeferred (agent-side). Live
/// activation skipped because a critical component (dbus/systemd/
/// kernel/init) cannot be live-swapped. Per the Option C lift, state
/// transitions Activating → Deferred — the distinct ordering-eligible-
/// but-not-health-verified state — instead of the pre-Option-C
/// "stays Activating" shape that blocked downstream gates. The
/// effect MUST emit OutboundAgentEvent::ActivationDeferred via the
/// LocalEmitEvent (durable) channel so CP sees the deferral in
/// event_log.
#[test]
fn local_activation_deferred_transitions_to_deferred_and_emits_outbound() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Activating);

    let (s_after, effects) = step(
        s,
        Event::LocalActivationDeferred {
            component: "systemd".into(),
            deferred_at: t0() + Duration::seconds(2),
            seq: 2,
        },
        t0() + Duration::seconds(2),
        &p,
    )
    .unwrap();

    assert_eq!(
        s_after.state,
        HostState::Deferred,
        "Activating → Deferred per Option C; cascade can progress past this host (host-edges + wave-promotion treat Deferred as ordering-eligible)",
    );
    assert_eq!(s_after.last_event_seq, 2, "last_event_seq advances");
    assert!(
        s_after.current_closure.is_none(),
        "current_closure stays None — the live switch did NOT take; the actual closure observation lands when LIFT #1's synthesis fires post-reboot",
    );
    assert!(
        s_after.activation_completed_at.is_none(),
        "activation_completed_at stays None — completion happens post-reboot via LIFT #1 synthesis (Deferred → Soaking)",
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::LocalEmitEvent {
                payload: OutboundAgentEvent::ActivationDeferred { component, .. },
                durable: true,
                ..
            } if component == "systemd"
        )),
        "MUST emit durable OutboundAgentEvent::ActivationDeferred for CP visibility"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RecordTransition {
                to: HostState::Deferred,
                ..
            }
        )),
        "MUST emit RecordTransition for Activating → Deferred"
    );
}

/// LIFT #2 + Option C: RemoteActivationDeferred (CP-side mirror).
/// Same Activating → Deferred shape as the Local variant, but the
/// CP-side path uses RemoteAppendEventLog (direct event_log write,
/// no outbound queue).
#[test]
fn remote_activation_deferred_transitions_to_deferred_and_appends_event_log() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::RemoteDispatchAck {
            current_closure_at_dispatch: "prior".into(),
            received_at: t0() + Duration::seconds(1),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Activating);

    let (s_after, effects) = step(
        s,
        Event::RemoteActivationDeferred {
            component: "dbus".into(),
            deferred_at: t0() + Duration::seconds(2),
            seq: 2,
        },
        t0() + Duration::seconds(2),
        &p,
    )
    .unwrap();

    assert_eq!(s_after.state, HostState::Deferred);
    assert_eq!(s_after.last_event_seq, 2);
    assert!(s_after.current_closure.is_none());
    assert!(s_after.activation_completed_at.is_none());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RemoteAppendEventLog {
                payload: OutboundAgentEvent::ActivationDeferred { component, .. },
                ..
            } if component == "dbus"
        )),
        "MUST emit RemoteAppendEventLog for CP-side event_log capture"
    );
}

/// LIFT #2 + LIFT #1 + Option C interaction: after the deferred
/// transition (state = Deferred), a subsequent RemoteActivationCompleted
/// (synthesized by LIFT #1 when the agent reports current == target
/// post-reboot) drives Deferred → Soaking via the new deferred.rs
/// handler. This pins the end-to-end path the operator follows when
/// they reboot a deferred-pending host.
#[test]
fn deferred_then_synthesized_completion_advances_to_soaking() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::RemoteDispatchAck {
            current_closure_at_dispatch: "prior".into(),
            received_at: t0() + Duration::seconds(1),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::RemoteActivationDeferred {
            component: "kernel".into(),
            deferred_at: t0() + Duration::seconds(2),
            seq: 2,
        },
        t0() + Duration::seconds(2),
        &p,
    )
    .unwrap();
    assert_eq!(s.state, HostState::Deferred);

    // Operator reboots → LIFT #1 synthesis on next heartbeat.
    let (s_after, _) = step(
        s,
        Event::RemoteActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::minutes(2),
            seq: 3,
        },
        t0() + Duration::minutes(2),
        &p,
    )
    .unwrap();
    assert_eq!(s_after.state, HostState::Soaking);
    assert_eq!(
        s_after.current_closure.as_deref(),
        Some("target-closure-abc"),
    );
    assert!(s_after.activation_completed_at.is_some());
}

/// Audit-trail regression: an evidence probe with per-control overrides
/// emits `LocalProbeResult` carrying `sub_results`, and the reducer
/// threads them onto the outbound `OutboundAgentEvent::ProbeResult` so
/// the signed event_log payload preserves the operator's override
/// reasons. Prior to this plumb-through the reducer dropped sub_results
/// at the Event boundary and unconditionally emitted `sub_results:
/// None`, leaving auditors with `subResults: null` on every signed row.
#[test]
fn local_probe_result_threads_sub_results_to_outbound_event() {
    let p = policy_halt();
    let s = pending();
    let (s, _) = step(
        s,
        Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq: 1,
        },
        t0() + Duration::seconds(1),
        &p,
    )
    .unwrap();
    let (s, _) = step(
        s,
        Event::LocalActivationCompleted {
            observed_current_closure: "target-closure-abc".into(),
            exit_code: 0,
            completed_at: t0() + Duration::seconds(5),
            seq: 2,
        },
        t0() + Duration::seconds(5),
        &p,
    )
    .unwrap();

    let subs = vec![
        ProbeSubResult {
            control_id: "encryption-at-rest".into(),
            status: ProbeStatus::Fail,
            framework: "nis2-essential".into(),
            article: Some("21(h)".into()),
            effective_mode: ProbeMode::Observe,
            override_reason: Some("Q2 audit window: observe-mode default".into()),
        },
        ProbeSubResult {
            control_id: "access-control".into(),
            status: ProbeStatus::Pass,
            framework: "nis2-essential".into(),
            article: Some("21(i)".into()),
            effective_mode: ProbeMode::Enforce,
            override_reason: None,
        },
    ];
    let (_s, effects) = step(
        s,
        Event::LocalProbeResult {
            probe_name: "evidence-nis2-essential".into(),
            mode: ProbeMode::Enforce,
            status: ProbeStatus::Fail,
            observed_at: t0() + Duration::seconds(10),
            failure_reason: Some("encryption-at-rest failed".into()),
            sub_results: Some(subs.clone()),
            seq: 3,
        },
        t0() + Duration::seconds(10),
        &p,
    )
    .unwrap();

    let emitted = effects
        .iter()
        .find_map(|e| match e {
            Effect::LocalEmitEvent {
                payload: OutboundAgentEvent::ProbeResult { sub_results, .. },
                ..
            } => Some(sub_results.clone()),
            _ => None,
        })
        .expect("LocalProbeResult must emit an OutboundAgentEvent::ProbeResult");
    assert_eq!(
        emitted,
        Some(subs),
        "sub_results must thread from Event → Effect verbatim",
    );
}
