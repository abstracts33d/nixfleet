//! Activation worker. Receives [`ActivationIntent`] from the applier,
//! emits `LocalActivationStarted` to the reducer, drives the rich
//! activation pipeline (`crate::activation`), and emits
//! `LocalActivationCompleted` / `LocalActivationFailed` based on the
//! pipeline's outcome.
//!
//! Test-mode (`NIXFLEET_AGENT_ACTIVATION_TEST_MODE`) short-circuits in
//! this worker BEFORE entering the pipeline — smoke tests + integration
//! tests rely on this to avoid spawning real subprocesses.

use chrono::Utc;
use nixfleet_proto::clock::ClockHandle;
use nixfleet_state_machine::Event;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::activation::{
    ActivationOutcome, ActivationTarget, RollbackOutcome, activate as run_pipeline,
    rollback as run_rollback_pipeline,
};

use super::super::wire::ActivationIntent;
use super::super::{AgentConfig, ReducerInput, ShutdownToken};

pub fn spawn(
    _cfg: AgentConfig,
    _clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    mut intent_rx: mpsc::Receiver<ActivationIntent>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_activation",
                        "task shut down",
                    );
                    return;
                }
                maybe = intent_rx.recv() => {
                    let Some(intent) = maybe else { return };
                    handle_intent(&input_tx, intent).await;
                }
            }
        }
    })
}

async fn handle_intent(input_tx: &mpsc::Sender<ReducerInput>, intent: ActivationIntent) {
    let started_at = Utc::now();
    // LOADBEARING: Failed → Activating is not a legal state-machine
    // transition; rollback drives Failed → Reverted directly via
    // `LocalRollbackCompleted` (RFC-0005 §3 / `failed.rs`). The forward
    // path emits `LocalActivationStarted` to stamp
    // `activation_started_at` (RFC-0005 §4.2); the rollback path skips
    // it — emitting an Activation* event from Failed state causes the
    // reducer to silently reject both LocalActivationStarted and
    // LocalActivationCompleted, blocking Failed → Reverted bookkeeping
    // and the quarantine populate.
    if !intent.rollback {
        let switch_method = "switch-to-configuration".to_string();
        if let Err(err) = input_tx
            .send(ReducerInput::HostEvent {
                rollout_id: intent.rollout_id.clone(),
                event: Event::LocalActivationStarted {
                    started_at,
                    switch_method,
                    seq: 0,
                },
            })
            .await
        {
            tracing::warn!(
                target: "agent_activation",
                error = %err,
                "reducer input channel closed during ActivationStarted",
            );
            return;
        }
    }

    let completed_event = if activation_test_mode_enabled() {
        // Test-mode short-circuit: no real subprocess, no /run/current-system
        // poll, no nix-store / nix-env invocation. Smoke + integration tests
        // depend on this to exercise the runtime integration end-to-end
        // through the durable queue without requiring a NixOS host.
        // Production code paths MUST NEVER set
        // `NIXFLEET_AGENT_ACTIVATION_TEST_MODE`; the gate is checked on
        // every intent so a misconfigured test environment fails closed
        // (no activation) rather than open (real subprocess).
        let now = Utc::now();
        tracing::info!(
            target: "agent_activation",
            target_closure = %intent.target_closure,
            rollback = intent.rollback,
            "activation: test-mode gate fired; skipping pipeline",
        );
        if intent.rollback {
            // Rollback path's test-mode synthesis: emit LocalRollbackCompleted
            // with the intent's target as the reverted-to closure (in test
            // mode there's no real /run/current-system to read).
            Event::LocalRollbackCompleted {
                reverted_to_closure: intent.target_closure.clone(),
                exit_code: 0,
                completed_at: now,
                seq: 0,
            }
        } else {
            Event::LocalActivationCompleted {
                observed_current_closure: intent.target_closure.clone(),
                exit_code: 0,
                completed_at: now,
                seq: 0,
            }
        }
    } else if intent.rollback {
        let now = Utc::now();
        match run_rollback_pipeline().await {
            Ok(RollbackOutcome::FiredAndPolled {
                reverted_to_closure,
            }) => {
                tracing::info!(
                    target: "agent_activation",
                    rollout_id = %intent.rollout_id,
                    %reverted_to_closure,
                    "activation: rollback pipeline completed; firing LocalRollbackCompleted",
                );
                // LOADBEARING: only `LocalRollbackCompleted` (not
                // `LocalActivationCompleted`) is legal from Failed
                // state per state-machine `failed.rs`. The
                // reverted_to_closure is the post-rollback
                // `/run/current-system` basename observed by
                // `verify_poll` in
                // `activation::rollback::rollback_with`.
                Event::LocalRollbackCompleted {
                    reverted_to_closure,
                    exit_code: 0,
                    completed_at: now,
                    seq: 0,
                }
            }
            Ok(RollbackOutcome::Failed { phase, exit_code }) => Event::LocalActivationFailed {
                exit_code: exit_code.unwrap_or(-1),
                stderr_tail: format!("rollback failed at phase {phase}"),
                failed_at: now,
                seq: 0,
            },
            Err(err) => Event::LocalActivationFailed {
                exit_code: -1,
                stderr_tail: format!("rollback pipeline error: {err}"),
                failed_at: now,
                seq: 0,
            },
        }
    } else {
        let target = ActivationTarget {
            closure_hash: intent.target_closure.clone(),
            channel_ref: intent.rollout_id.channel_ref().to_string(),
        };
        let now = Utc::now();
        match run_pipeline(&target).await {
            Ok(ActivationOutcome::FiredAndPolled) => {
                tracing::info!(
                    target: "agent_activation",
                    rollout_id = %intent.rollout_id,
                    target_closure = %intent.target_closure,
                    "activation: pipeline completed (FiredAndPolled)",
                );
                Event::LocalActivationCompleted {
                    // FiredAndPolled means verify_poll observed
                    // /run/current-system match the expected target — so
                    // observed == target by construction at this point.
                    observed_current_closure: intent.target_closure.clone(),
                    exit_code: 0,
                    completed_at: now,
                    seq: 0,
                }
            }
            Ok(ActivationOutcome::DeferredPendingReboot { component }) => {
                // Profile set; bootloader updated; live switch deferred
                // because `component` (dbus/systemd/kernel/init) cannot
                // be live-swapped per nixos-rebuild's own rules. New
                // generation activates on next reboot.
                //
                // LIFT #2: emit `LocalActivationDeferred` (state stays
                // Activating, no fake exit_code=0, no fake
                // observed_current_closure). On next reboot the agent's
                // boot-recovery handshake observes current == target
                // and CP's handle_heartbeat (LIFT #1) synthesizes the
                // RemoteActivationCompleted that advances the cascade.
                //
                // Pre-LIFT-#2 this emitted LocalActivationCompleted with
                // `exit_code: 0` (a lie — activation didn't take live)
                // and `observed_current_closure: <pre-switch closure or
                // fallback to target>` (also a lie — the observation
                // pointed at the wrong thing). The state machine then
                // transitioned Activating → Soaking with current_closure
                // ≠ target_closure, producing a soft-converged state
                // that misled operators about the reboot requirement.
                tracing::warn!(
                    target: "agent_activation",
                    rollout_id = %intent.rollout_id,
                    target_closure = %intent.target_closure,
                    component = %component,
                    "activation: deferred pending reboot (critical component swap); bootloader updated, host stays at Activating until reboot triggers LIFT #1's retroactive confirmation",
                );
                Event::LocalActivationDeferred {
                    component,
                    deferred_at: now,
                    seq: 0,
                }
            }
            Ok(ActivationOutcome::SignatureMismatch {
                closure_hash,
                stderr_tail,
            }) => {
                tracing::error!(
                    target: "agent_activation",
                    rollout_id = %intent.rollout_id,
                    %closure_hash,
                    "activation: signature mismatch — closure refused by substituter trust",
                );
                Event::LocalActivationFailed {
                    // Distinct exit_code -2 lets dashboards route trust
                    // violations separately from generic switch failures.
                    exit_code: -2,
                    stderr_tail: format!("signature mismatch on {closure_hash}: {stderr_tail}"),
                    failed_at: now,
                    seq: 0,
                }
            }
            Ok(ActivationOutcome::RealiseFailed { reason }) => Event::LocalActivationFailed {
                exit_code: -1,
                stderr_tail: format!("realise failed: {reason}"),
                failed_at: now,
                seq: 0,
            },
            Ok(ActivationOutcome::SwitchFailed { phase, exit_code }) => {
                Event::LocalActivationFailed {
                    exit_code: exit_code.unwrap_or(-1),
                    stderr_tail: format!("switch failed at phase {phase}"),
                    failed_at: now,
                    seq: 0,
                }
            }
            Ok(ActivationOutcome::VerifyMismatch { expected, actual }) => {
                tracing::error!(
                    target: "agent_activation",
                    rollout_id = %intent.rollout_id,
                    %expected,
                    %actual,
                    "activation: /run/current-system flipped to unexpected basename",
                );
                Event::LocalActivationFailed {
                    // Distinct exit_code -3 surfaces verify-mismatch as a
                    // rollback-required class (third closure observed).
                    exit_code: -3,
                    stderr_tail: format!("verify mismatch: expected={expected} actual={actual}",),
                    failed_at: now,
                    seq: 0,
                }
            }
            Err(err) => Event::LocalActivationFailed {
                exit_code: -1,
                stderr_tail: format!("activation pipeline error: {err}"),
                failed_at: now,
                seq: 0,
            },
        }
    };

    if let Err(err) = input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: intent.rollout_id,
            event: completed_event,
        })
        .await
    {
        tracing::warn!(
            target: "agent_activation",
            error = %err,
            "reducer input channel closed during ActivationCompleted/Failed",
        );
    }
}

/// Test-mode gate. When the env var `NIXFLEET_AGENT_ACTIVATION_TEST_MODE`
/// is set to ANY value, `handle_intent` short-circuits the activation
/// pipeline and emits `LocalActivationCompleted` with the intent's
/// target as the observed value. Smoke tests
/// (`tests/runtime_smoke.rs`) MUST set this — they exercise the runtime
/// integration end-to-end through the durable queue, not the actual
/// activation subprocess. Production code paths must NEVER set it; the
/// gate is read on every intent so a test setup mistake fails closed
/// (no activation) rather than open (real subprocess).
fn activation_test_mode_enabled() -> bool {
    std::env::var_os("NIXFLEET_AGENT_ACTIVATION_TEST_MODE").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage of the test-mode gate at the worker layer. The activation
    /// pipeline's unit tests live in `crate::activation::*::tests`;
    /// integration tests in `tests/runtime_smoke.rs` exercise the
    /// short-circuited path end-to-end through the reducer + outbound
    /// queue. This minimal worker-layer test pins the env-var contract.
    #[test]
    fn test_mode_gate_responds_to_env_var() {
        let key = "NIXFLEET_AGENT_ACTIVATION_TEST_MODE";
        // SAFETY: env mutation is process-global; this test reads its
        // current value and restores it. Test framework runs tests in
        // separate threads but env-var read/write is unsafe under
        // concurrent mutation. We're not racing against a writer here —
        // this is the only test that touches this var — but the
        // remove + set + restore sequence is wrapped in `unsafe`
        // explicitly per std::env::set_var's safety docs.
        let prior = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        assert!(!activation_test_mode_enabled());
        unsafe {
            std::env::set_var(key, "1");
        }
        assert!(activation_test_mode_enabled());
        // Restore prior state so other tests in the same process don't see
        // a leaked env var.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    /// Regression guard for the rollback path's event-type contract.
    /// The state-machine handler at `failed.rs:29-62` legalises only
    /// `LocalRollbackCompleted` and `RemoteRollbackComplete` from
    /// Failed state; emitting `LocalActivationStarted` or
    /// `LocalActivationCompleted` instead would be rejected by the
    /// reducer as "illegal transition", silently blocking Failed →
    /// Reverted and the quarantine populate.
    ///
    /// The rollback path:
    ///   (a) skips `LocalActivationStarted` (rollback doesn't start
    ///       activation)
    ///   (b) emits `LocalRollbackCompleted { reverted_to_closure, ... }`
    ///       on success — handler sets state.state = Reverted, populates
    ///       `reverted_at` + `reverted_to`, and emits
    ///       `OutboundAgentEvent::RollbackComplete` for CP propagation
    ///       (driving CP's `Effect::RemoteInsertQuarantine` on the bad
    ///       SHA).
    ///
    /// This test pins the worker's test-mode short-circuit path for
    /// rollback intents. The production pipeline path is the same code
    /// branch with real `run_rollback_pipeline()` instead of the test
    /// gate.
    #[tokio::test]
    async fn rollback_intent_emits_local_rollback_completed_not_activation_completed() {
        use crate::runtime::wire::ActivationIntent;
        use nixfleet_proto::RolloutId;
        use nixfleet_state_machine::Event;
        use tokio::sync::mpsc;

        let key = "NIXFLEET_AGENT_ACTIVATION_TEST_MODE";
        let prior = std::env::var_os(key);
        // SAFETY: see test_mode_gate_responds_to_env_var.
        unsafe {
            std::env::set_var(key, "1");
        }

        let (tx, mut rx) = mpsc::channel::<ReducerInput>(8);
        let intent = ActivationIntent {
            rollout_id: RolloutId::new("edge", "abc1234deadbeef"),
            target_closure: "rollback-target-closure".to_string(),
            rollback: true,
        };

        handle_intent(&tx, intent).await;

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Pre-fix this would have emitted [LocalActivationStarted,
        // LocalActivationCompleted]. Post-fix it must emit exactly
        // [LocalRollbackCompleted] — no Activation* events.
        assert_eq!(
            events.len(),
            1,
            "rollback path must emit exactly one event (LocalRollbackCompleted); got {} events",
            events.len(),
        );
        let ReducerInput::HostEvent { event, .. } = events.into_iter().next().unwrap() else {
            panic!("expected HostEvent variant");
        };
        match event {
            Event::LocalRollbackCompleted {
                reverted_to_closure,
                ..
            } => {
                assert_eq!(
                    reverted_to_closure, "rollback-target-closure",
                    "reverted_to_closure should carry the post-rollback closure (in test-mode short-circuit, the intent's target is used)",
                );
            }
            other => {
                panic!("rollback path must emit LocalRollbackCompleted; got: {other:?}")
            }
        }

        // Restore env state.
        unsafe {
            match prior {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
