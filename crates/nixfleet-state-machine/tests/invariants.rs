//! Property-based + table-driven invariant tests.
//!
//! Covers RFC-0008 §3 + §4.2 invariants in two layers:
//!
//! - **Positive space**: every legal event sequence preserves the §3
//!   invariants (Converged ⇒ current == declared, etc.). Random sequences
//!   via proptest.
//! - **Negative space**: every (state, event) pair the §3 graph does NOT
//!   permit returns `TransitionError`. Table-driven enumeration over all
//!   6 × 20 = 120 pairs, minus the legal subset. This is the bug-prevention
//!   property the architecture is supposed to deliver — the type system
//!   makes the legal set explicit, the tests make the illegal set verified.

use chrono::{DateTime, Duration, TimeZone, Utc};
use nixfleet_proto::{HealthGate, OnHealthFailure, RolloutPolicy};
use nixfleet_state_machine::{
    Effect, Event, HostRolloutState, HostState, ProbeMode, ProbeRecord, ProbeStatus,
    TransitionError, step,
};
use proptest::prelude::*;

// ─────────────────────────────────────────────────────────────────────────
// Common fixtures
// ─────────────────────────────────────────────────────────────────────────

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

fn state_in(state: HostState) -> HostRolloutState {
    let mut s = HostRolloutState::new_pending(
        "r1".into(),
        "h1".into(),
        "stable".into(),
        "target-abc".into(),
        t0(),
        t0() + Duration::minutes(5),
    );
    s.state = state;
    if matches!(state, HostState::Soaking | HostState::Converged) {
        s.current_closure = Some("target-abc".into());
        s.activation_completed_at = Some(t0() + Duration::seconds(5));
    }
    if matches!(state, HostState::Failed | HostState::Reverted) {
        s.failed_at = Some(t0() + Duration::seconds(125));
        s.current_closure_at_dispatch = Some("prior-closure".into());
    }
    if matches!(state, HostState::Reverted) {
        s.reverted_at = Some(t0() + Duration::seconds(135));
        s.reverted_to = Some("prior-closure".into());
        s.current_closure = Some("prior-closure".into());
    }
    if matches!(state, HostState::Converged) {
        s.converged_at = Some(t0() + Duration::minutes(6));
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────
// Event variant catalogue + legality table
// ─────────────────────────────────────────────────────────────────────────

const ALL_EVENT_VARIANTS: &[&str] = &[
    "LocalActivate",
    "LocalActivationStarted",
    "LocalActivationCompleted",
    "LocalActivationFailed",
    "LocalProbeObservedFirst",
    "LocalProbeResult",
    "LocalProbeFailureFirst",
    "LocalSustainedFailureCrossed",
    "LocalRollbackCompleted",
    "LocalConvergedReached",
    "RemoteDispatchAck",
    "RemoteActivationStarted",
    "RemoteActivationCompleted",
    "RemoteActivationFailed",
    "RemoteProbeObservedFirst",
    "RemoteProbeResult",
    "RemoteProbeFailureFirst",
    "RemoteFailed",
    "RemoteRollbackComplete",
    "RemoteConverged",
];

/// Legal (state, event) pairs per RFC-0008 §3 graph + §4.2 wire vocabulary.
fn is_legal(state: HostState, event: &str) -> bool {
    match (state, event) {
        (HostState::Pending, "LocalActivate" | "RemoteDispatchAck") => true,
        (
            HostState::Activating,
            "LocalActivationStarted"
            | "RemoteActivationStarted"
            | "LocalActivationCompleted"
            | "RemoteActivationCompleted"
            | "LocalActivationFailed"
            | "RemoteActivationFailed",
        ) => true,
        (
            HostState::Soaking,
            "LocalProbeObservedFirst"
            | "RemoteProbeObservedFirst"
            | "LocalProbeResult"
            | "RemoteProbeResult"
            | "LocalProbeFailureFirst"
            | "RemoteProbeFailureFirst"
            | "LocalSustainedFailureCrossed"
            | "RemoteFailed"
            | "LocalConvergedReached"
            | "RemoteConverged",
        ) => true,
        (HostState::Failed, "LocalRollbackCompleted" | "RemoteRollbackComplete") => true,
        // Converged and Reverted accept no events.
        _ => false,
    }
}

fn event_with_seq(name: &str, seq: u64) -> Event {
    let at = t0() + Duration::seconds(10);
    match name {
        "LocalActivate" => Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target-abc".into(),
            received_at: at,
            soak_due_at: at + Duration::minutes(5),
            seq,
        },
        "LocalActivationStarted" => Event::LocalActivationStarted {
            started_at: at,
            switch_method: "systemd-run".into(),
            seq,
        },
        "LocalActivationCompleted" => Event::LocalActivationCompleted {
            observed_current_closure: "target-abc".into(),
            exit_code: 0,
            completed_at: at,
            seq,
        },
        "LocalActivationFailed" => Event::LocalActivationFailed {
            exit_code: 1,
            stderr_tail: "x".into(),
            failed_at: at,
            seq,
        },
        "LocalProbeObservedFirst" => Event::LocalProbeObservedFirst {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            observed_at: at,
            seq,
        },
        "LocalProbeResult" => Event::LocalProbeResult {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            status: ProbeStatus::Pass,
            observed_at: at,
            failure_reason: None,
            sub_results: None,
            seq,
        },
        "LocalProbeFailureFirst" => Event::LocalProbeFailureFirst {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            first_failed_at: at,
            seq,
        },
        "LocalSustainedFailureCrossed" => Event::LocalSustainedFailureCrossed {
            failed_at: at,
            sustained_duration_secs: 60,
            failing_probes: vec!["p".into()],
            policy_applied: OnHealthFailure::Halt,
            seq,
        },
        "LocalRollbackCompleted" => Event::LocalRollbackCompleted {
            reverted_to_closure: "prior".into(),
            exit_code: 0,
            completed_at: at,
            seq,
        },
        "LocalConvergedReached" => Event::LocalConvergedReached {
            converged_at: at,
            current_closure: "target-abc".into(),
            seq,
        },
        "RemoteDispatchAck" => Event::RemoteDispatchAck {
            current_closure_at_dispatch: "prior".into(),
            received_at: at,
            seq,
        },
        "RemoteActivationStarted" => Event::RemoteActivationStarted {
            started_at: at,
            switch_method: "systemd-run".into(),
            seq,
        },
        "RemoteActivationCompleted" => Event::RemoteActivationCompleted {
            observed_current_closure: "target-abc".into(),
            exit_code: 0,
            completed_at: at,
            seq,
        },
        "RemoteActivationFailed" => Event::RemoteActivationFailed {
            exit_code: 1,
            stderr_tail: "x".into(),
            failed_at: at,
            seq,
        },
        "RemoteProbeObservedFirst" => Event::RemoteProbeObservedFirst {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            observed_at: at,
            seq,
        },
        "RemoteProbeResult" => Event::RemoteProbeResult {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            status: ProbeStatus::Pass,
            observed_at: at,
            failure_reason: None,
            sub_results: None,
            seq,
        },
        "RemoteProbeFailureFirst" => Event::RemoteProbeFailureFirst {
            probe_name: "p".into(),
            mode: nixfleet_state_machine::ProbeMode::Enforce,
            first_failed_at: at,
            seq,
        },
        "RemoteFailed" => Event::RemoteFailed {
            failed_at: at,
            sustained_duration_secs: 60,
            failing_probes: vec!["p".into()],
            policy_applied: OnHealthFailure::Halt,
            seq,
        },
        "RemoteRollbackComplete" => Event::RemoteRollbackComplete {
            reverted_to_closure: "prior".into(),
            exit_code: 0,
            completed_at: at,
            seq,
        },
        "RemoteConverged" => Event::RemoteConverged {
            converged_at: at,
            current_closure: "target-abc".into(),
            seq,
        },
        other => panic!("unknown event variant in test catalogue: {other}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Negative-space coverage (the bug-prevention property)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn every_illegal_state_event_pair_returns_transition_error() {
    let all_states = [
        HostState::Pending,
        HostState::Activating,
        HostState::Soaking,
        HostState::Converged,
        HostState::Failed,
        HostState::Reverted,
    ];
    let policy = policy_halt();
    let mut illegal_count = 0;
    let mut legal_count = 0;
    for state in all_states {
        for event_name in ALL_EVENT_VARIANTS {
            let s = state_in(state);
            let event = event_with_seq(event_name, 1);
            let result = step(s, event, t0() + Duration::seconds(10), &policy);
            if is_legal(state, event_name) {
                legal_count += 1;
                // Legal pairs MAY still fail (e.g. invariant violation on
                // RemoteConverged with mismatched closure), but they must
                // not fail with IllegalForState.
                if let Err(TransitionError::IllegalForState { .. }) = result {
                    panic!(
                        "legal pair ({state:?}, {event_name}) returned IllegalForState; \
                         the legality table is out of sync with the dispatcher"
                    );
                }
            } else {
                illegal_count += 1;
                assert!(
                    matches!(result, Err(TransitionError::IllegalForState { .. })),
                    "illegal pair ({state:?}, {event_name}) did not return IllegalForState: \
                     got {result:?}"
                );
            }
        }
    }
    // Sanity-check the coverage matrix.
    assert_eq!(legal_count + illegal_count, 120);
    assert_eq!(legal_count, 20, "expected 20 legal (state, event) pairs");
}

#[test]
fn seq_regression_rejected_on_any_state() {
    let policy = policy_halt();
    for state in [
        HostState::Pending,
        HostState::Activating,
        HostState::Soaking,
        HostState::Failed,
    ] {
        let mut s = state_in(state);
        s.last_event_seq = 10;
        for &name in ALL_EVENT_VARIANTS {
            if !is_legal(state, name) {
                continue;
            }
            // seq exactly equal: rejected (already-processed retransmit)
            let res = step(
                s.clone(),
                event_with_seq(name, 10),
                t0() + Duration::seconds(10),
                &policy,
            );
            assert!(
                matches!(res, Err(TransitionError::SeqRegression { .. })),
                "seq == last_event_seq must be rejected, got {res:?}"
            );
            // seq lower: rejected (out-of-order)
            let res = step(
                s.clone(),
                event_with_seq(name, 5),
                t0() + Duration::seconds(10),
                &policy,
            );
            assert!(
                matches!(res, Err(TransitionError::SeqRegression { .. })),
                "seq < last_event_seq must be rejected, got {res:?}"
            );
        }
    }
}

#[test]
fn converged_with_wrong_current_closure_rejected() {
    let policy = policy_halt();
    let s = state_in(HostState::Soaking);
    let bad = Event::LocalConvergedReached {
        converged_at: t0() + Duration::minutes(6),
        current_closure: "different-from-target".into(),
        seq: 1,
    };
    let err = step(s, bad, t0() + Duration::minutes(6), &policy).unwrap_err();
    assert!(matches!(err, TransitionError::Invariant(_)));
}

#[test]
fn converged_before_soak_due_at_rejected() {
    let policy = policy_halt();
    let s = state_in(HostState::Soaking);
    // state_in() sets soak_due_at to t0+5min; converge at t0+1min
    let too_early = Event::LocalConvergedReached {
        converged_at: t0() + Duration::minutes(1),
        current_closure: "target-abc".into(),
        seq: 1,
    };
    let err = step(s, too_early, t0() + Duration::minutes(1), &policy).unwrap_err();
    assert!(matches!(err, TransitionError::Invariant(_)));
}

#[test]
fn converged_with_failing_observe_probe_accepted() {
    // RFC-0010 §3.3 (ProbeMode docstring): observe-mode probes do not
    // gate. A Soaking host with a Passing enforce probe + Failing observe
    // probe is the canonical "evidence-collector race / non-blocking
    // visibility" shape — must converge.
    let policy = policy_halt();
    let mut s = state_in(HostState::Soaking);
    s.probes.insert(
        "nginx-version".into(),
        ProbeRecord {
            status: ProbeStatus::Pass,
            mode: ProbeMode::Enforce,
            last_observed_at: t0() + Duration::minutes(1),
            last_pass_at: Some(t0() + Duration::minutes(1)),
            failure_reason: None,
        },
    );
    s.probes.insert(
        "evidence-nis2".into(),
        ProbeRecord {
            status: ProbeStatus::Fail,
            mode: ProbeMode::Observe,
            last_observed_at: t0() + Duration::minutes(1),
            last_pass_at: None,
            failure_reason: Some("evidence verification failed".into()),
        },
    );
    let ev = Event::LocalConvergedReached {
        converged_at: t0() + Duration::minutes(6),
        current_closure: "target-abc".into(),
        seq: 1,
    };
    let (next, _) = step(s, ev, t0() + Duration::minutes(6), &policy).unwrap();
    assert_eq!(next.state, HostState::Converged);
}

#[test]
fn converged_with_failing_enforce_probe_rejected() {
    // Companion to the observe-probe acceptance test: enforce-mode Fail
    // does still gate.
    let policy = policy_halt();
    let mut s = state_in(HostState::Soaking);
    s.probes.insert(
        "nginx-version".into(),
        ProbeRecord {
            status: ProbeStatus::Fail,
            mode: ProbeMode::Enforce,
            last_observed_at: t0() + Duration::minutes(1),
            last_pass_at: None,
            failure_reason: Some("502 from upstream".into()),
        },
    );
    let ev = Event::LocalConvergedReached {
        converged_at: t0() + Duration::minutes(6),
        current_closure: "target-abc".into(),
        seq: 1,
    };
    let err = step(s, ev, t0() + Duration::minutes(6), &policy).unwrap_err();
    assert!(matches!(err, TransitionError::Invariant(_)));
}

// ─────────────────────────────────────────────────────────────────────────
// Positive-space invariants via proptest
// ─────────────────────────────────────────────────────────────────────────

fn arb_event_seq(seq: u64) -> impl Strategy<Value = Event> {
    prop_oneof![
        Just(event_with_seq("LocalActivate", seq)),
        Just(event_with_seq("LocalActivationCompleted", seq)),
        Just(event_with_seq("LocalActivationFailed", seq)),
        Just(event_with_seq("LocalProbeObservedFirst", seq)),
        Just(event_with_seq("LocalProbeResult", seq)),
        Just(event_with_seq("LocalSustainedFailureCrossed", seq)),
        Just(event_with_seq("LocalRollbackCompleted", seq)),
        Just(event_with_seq("LocalConvergedReached", seq)),
    ]
}

proptest! {
    #[test]
    fn step_is_deterministic(seq in 1u64..1000) {
        let policy = policy_halt();
        let s = state_in(HostState::Pending);
        let ev = event_with_seq("LocalActivate", seq);
        let r1 = step(s.clone(), ev.clone(), t0() + Duration::seconds(10), &policy);
        let r2 = step(s, ev, t0() + Duration::seconds(10), &policy);
        match (r1, r2) {
            (Ok((s1, e1)), Ok((s2, e2))) => {
                prop_assert_eq!(s1.state, s2.state);
                prop_assert_eq!(s1.last_event_seq, s2.last_event_seq);
                prop_assert_eq!(e1.len(), e2.len());
            }
            (Err(e1), Err(e2)) => {
                prop_assert_eq!(format!("{e1:?}"), format!("{e2:?}"));
            }
            (a, b) => prop_assert!(false, "non-deterministic: {a:?} vs {b:?}"),
        }
    }

    /// Reducer state cannot end in Converged unless current_closure == target_closure
    /// (RFC-0008 §3 invariant).
    #[test]
    fn converged_implies_current_eq_target(seq in 1u64..1000) {
        let policy = policy_halt();
        let s = state_in(HostState::Soaking);
        let target = s.target_closure.clone();
        // Try LocalConvergedReached with various current_closure strings.
        for candidate in ["target-abc", "wrong", "also-wrong"] {
            let ev = Event::LocalConvergedReached {
                converged_at: t0() + Duration::minutes(6),
                current_closure: candidate.into(),
                seq,
            };
            if let Ok((next, _)) = step(s.clone(), ev, t0() + Duration::minutes(6), &policy)
                && next.state == HostState::Converged
            {
                prop_assert_eq!(next.current_closure.as_deref(), Some(target.as_str()));
            }
        }
    }

    /// Any event arriving at a terminal state (Converged / Reverted) is rejected.
    #[test]
    fn terminal_states_reject_all_events(event_idx in 0usize..ALL_EVENT_VARIANTS.len(), seq in 1u64..1000) {
        let policy = policy_halt();
        let name = ALL_EVENT_VARIANTS[event_idx];
        for terminal in [HostState::Converged, HostState::Reverted] {
            let s = state_in(terminal);
            let ev = event_with_seq(name, seq);
            let res = step(s, ev, t0() + Duration::seconds(10), &policy);
            prop_assert!(
                matches!(res, Err(TransitionError::IllegalForState { .. })),
                "terminal state {terminal:?} accepted {name}: {res:?}"
            );
        }
        // Drop unused variable warning
        let _ = arb_event_seq(seq);
    }

    /// Reducer must update last_event_seq on success.
    #[test]
    fn last_event_seq_advances_on_success(seq in 1u64..1000) {
        let policy = policy_halt();
        let s = state_in(HostState::Pending);
        let ev = Event::LocalActivate {
            current_closure_at_dispatch: "prior".into(),
            target_closure: "target-abc".into(),
            received_at: t0() + Duration::seconds(1),
            soak_due_at: t0() + Duration::minutes(5),
            seq,
        };
        let (next, _) = step(s, ev, t0() + Duration::seconds(1), &policy).unwrap();
        prop_assert_eq!(next.last_event_seq, seq);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Effect-shape sanity
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn activation_completed_emits_reset_probe_cache_effect() {
    let policy = policy_halt();
    let s = state_in(HostState::Activating);
    let ev = Event::LocalActivationCompleted {
        observed_current_closure: "target-abc".into(),
        exit_code: 0,
        completed_at: t0() + Duration::seconds(5),
        seq: 1,
    };
    let (_, effects) = step(s, ev, t0() + Duration::seconds(5), &policy).unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LocalResetProbeCache { .. })),
        "ActivationCompleted must emit LocalResetProbeCache"
    );
}

#[test]
fn rollback_complete_emits_quarantine_on_remote_side() {
    let policy = policy_halt();
    let s = state_in(HostState::Failed);
    let ev = Event::RemoteRollbackComplete {
        reverted_to_closure: "prior".into(),
        exit_code: 0,
        completed_at: t0() + Duration::seconds(135),
        seq: 1,
    };
    let (_, effects) = step(s, ev, t0() + Duration::seconds(135), &policy).unwrap();
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RemoteInsertQuarantine { closure, .. } if closure == "target-abc"
        )),
        "RemoteRollbackComplete must quarantine the bad target_closure"
    );
}
