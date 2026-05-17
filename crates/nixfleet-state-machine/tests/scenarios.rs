//! Happy-path + critical-failure scenarios for the per-host reducer.
//! Property tests in `invariants.rs` cover the broader space via proptest.

use chrono::{DateTime, Duration, TimeZone, Utc};
use nixfleet_proto::{HealthGate, OnHealthFailure, RolloutPolicy};
use nixfleet_state_machine::{
    Effect, Event, HostRolloutState, HostState, OutboundAgentEvent, ProbeStatus, TransitionError,
    step,
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
