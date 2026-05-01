//! Dispatch path: `process_dispatch_target` + the `handle_*`
//! family. Each function consumes a CP-issued target (or one of
//! `activation::ActivationOutcome`'s failure variants) and either
//! calls the wire helpers via the [`Reporter`] trait or chains into
//! `activation` / `compliance` / `manifest_cache`.
//!
//! Lives as a binary-local module (`mod dispatch;` in main.rs)
//! because some handlers still depend on `super::Args` (the
//! clap-parsed agent CLI struct) for state-dir + compliance-mode.
//! The CP-bound side-effects route through `&impl Reporter`, so
//! handlers are unit-testable with a capturing fake — see
//! `handle_signature_mismatch_posts_signed_event_and_does_not_attempt_rollback`
//! in this file's tests.
//!
//! Only two leaf functions still take a raw `&reqwest::Client`:
//! `process_dispatch_target` (manifest fetch) and
//! `confirm_and_finalize` (POST /v1/agent/confirm). Everything else
//! is reporter-only.

use nixfleet_proto::agent_wire::ReportEvent;

use nixfleet_agent::comms::Reporter;

use super::Args;

pub(crate) async fn process_dispatch_target(
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    client: &reqwest::Client,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    use nixfleet_agent::freshness::{check as freshness_check, FreshnessCheck};
    match freshness_check(target, chrono::Utc::now()) {
        FreshnessCheck::Stale {
            signed_at,
            freshness_window_secs,
            age_secs,
        } => {
            tracing::warn!(
                closure_hash = %target.closure_hash,
                channel_ref = %target.channel_ref,
                signed_at = %signed_at,
                freshness_window_secs,
                age_secs,
                "agent: refusing stale target — fleet.resolved older than freshness_window + 60s slack",
            );
            let stale_payload = nixfleet_agent::evidence_signer::StaleTargetSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                closure_hash: &target.closure_hash,
                channel_ref: &target.channel_ref,
                signed_at,
                freshness_window_secs,
                age_secs,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&stale_payload).ok());
            reporter
                .post_report(
                    Some(&target.channel_ref),
                    ReportEvent::StaleTarget {
                        closure_hash: target.closure_hash.clone(),
                        channel_ref: target.channel_ref.clone(),
                        signed_at,
                        freshness_window_secs,
                        age_secs,
                        signature,
                    },
                )
                .await;
            return;
        }
        FreshnessCheck::Unknown => {
            tracing::debug!(
                closure_hash = %target.closure_hash,
                "agent: target lacks signed_at/freshness_window_secs — older CP, skipping freshness gate",
            );
        }
        FreshnessCheck::Fresh => {}
    }

    // Manifest gate (RFC-0002 §4.4 / RFC-0003 §4.1): the agent MUST
    // fetch + verify the rollout manifest from the CP, recompute its
    // content hash, and assert (hostname, wave_index) ∈ host_set
    // before consuming any other field of `target`. Failure on any
    // step is hard refuse-to-act with a signed event.
    if let Some(rollout_id) = target.rollout_id.as_deref() {
        let cache = nixfleet_agent::manifest_cache::ManifestCache::new(
            &args.state_dir,
            &args.trust_file,
        );
        let wave_index = target.wave_index.unwrap_or(0);
        match cache
            .ensure(client, &args.control_plane_url, rollout_id, &args.machine_id, wave_index)
            .await
        {
            Ok(_manifest) => {
                tracing::debug!(
                    rollout_id = %rollout_id,
                    wave_index = wave_index,
                    "agent: rollout manifest verified",
                );
            }
            Err(err) => {
                handle_manifest_error(
                    err,
                    rollout_id,
                    target,
                    reporter,
                    args,
                    evidence_signer,
                )
                .await;
                return;
            }
        }
    } else {
        tracing::debug!(
            closure_hash = %target.closure_hash,
            "agent: target lacks rollout_id — older CP, skipping manifest gate",
        );
    }

    // Best-effort. Failure means the next regular checkin
    // re-dispatches instead of boot-recovery confirming.
    let dispatch_record = nixfleet_agent::checkin_state::LastDispatchRecord {
        closure_hash: target.closure_hash.clone(),
        channel_ref: target.channel_ref.clone(),
        rollout_id: target.rollout_id.clone(),
        dispatched_at: chrono::Utc::now(),
    };
    if let Err(err) =
        nixfleet_agent::checkin_state::write_last_dispatched(&args.state_dir, &dispatch_record)
    {
        tracing::warn!(
            error = %err,
            state_dir = %args.state_dir.display(),
            "write_last_dispatched failed; boot-recovery path will fall back to next-checkin re-dispatch",
        );
    }

    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::ActivationStarted {
                closure_hash: target.closure_hash.clone(),
                channel_ref: target.channel_ref.clone(),
            },
        )
        .await;

    let outcome = nixfleet_agent::activation::activate(target).await;
    handle_activation_outcome(outcome, target, reporter, client, args, evidence_signer).await;
}

/// Dispatch on the result of `activation::activate`. Telemetry-only
/// failures are logged, never propagated.
async fn handle_activation_outcome(
    outcome: anyhow::Result<nixfleet_agent::activation::ActivationOutcome>,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    client_handle: &reqwest::Client,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    use nixfleet_agent::activation::ActivationOutcome;
    match outcome {
        Ok(ActivationOutcome::FiredAndPolled) => {
            handle_fired_and_polled(target, reporter, client_handle, args, evidence_signer).await;
        }
        Ok(ActivationOutcome::RealiseFailed { reason }) => {
            handle_realise_failed(reason, target, reporter, args, evidence_signer).await;
        }
        Ok(ActivationOutcome::SignatureMismatch {
            closure_hash,
            stderr_tail,
        }) => {
            handle_signature_mismatch(
                closure_hash,
                stderr_tail,
                target,
                reporter,
                args,
                evidence_signer,
            )
            .await;
        }
        Ok(ActivationOutcome::SwitchFailed { phase, exit_code }) => {
            handle_switch_failed(phase, exit_code, target, reporter, args, evidence_signer).await;
        }
        Ok(ActivationOutcome::VerifyMismatch { expected, actual }) => {
            handle_verify_mismatch(expected, actual, target, reporter, args, evidence_signer).await;
        }
        Err(err) => {
            handle_activation_spawn_error(err, target, reporter).await;
        }
    }
}

/// Switch fired and polled successfully → run the runtime compliance
/// gate, then either confirm with the CP or roll back depending on
/// the gate outcome.
async fn handle_fired_and_polled(
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    client_handle: &reqwest::Client,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    let activation_completed_at = chrono::Utc::now();
    let (resolved_mode, gate_outcome) = run_runtime_gate(target, args, activation_completed_at).await;
    let gate_blocks_confirm = process_gate_outcome(
        &gate_outcome,
        resolved_mode,
        target,
        reporter,
        args,
        evidence_signer,
        activation_completed_at,
    )
    .await;
    if gate_blocks_confirm {
        return;
    }
    confirm_and_finalize(target, reporter, client_handle, args, evidence_signer).await;
}

/// Resolve the effective compliance mode (CP channel policy beats
/// the agent's CLI default) and run the runtime gate.
async fn run_runtime_gate(
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    args: &Args,
    activation_completed_at: chrono::DateTime<chrono::Utc>,
) -> (
    nixfleet_agent::compliance::GateMode,
    nixfleet_agent::compliance::GateOutcome,
) {
    use nixfleet_agent::compliance::GateMode;
    let cli_default_mode = args
        .compliance_gate_mode
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "auto")
        .map(GateMode::from_wire_str);
    let input_mode = target
        .compliance_mode
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "auto")
        .map(GateMode::from_wire_str)
        .or(cli_default_mode);
    let resolved_mode = nixfleet_agent::compliance::resolve_mode(input_mode).await;
    let gate_outcome = nixfleet_agent::compliance::run_runtime_gate(
        activation_completed_at,
        &nixfleet_agent::compliance::default_evidence_path(),
        resolved_mode,
    )
    .await;
    (resolved_mode, gate_outcome)
}

/// Post events for the gate outcome; return true iff the agent
/// should skip confirm and stay on the rolled-back generation.
async fn process_gate_outcome(
    gate_outcome: &nixfleet_agent::compliance::GateOutcome,
    resolved_mode: nixfleet_agent::compliance::GateMode,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
    activation_completed_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    use nixfleet_agent::compliance::GateOutcome;
    match gate_outcome {
        GateOutcome::Pass { .. } => {
            tracing::info!("compliance gate: PASS (all controls compliant)");
            false
        }
        GateOutcome::Skipped { reason } => {
            tracing::debug!(%reason, ?resolved_mode, "compliance gate: skipped");
            false
        }
        GateOutcome::Failures { evidence, failures } => {
            post_compliance_failures(failures, evidence, target, reporter, args, evidence_signer)
                .await;
            false
        }
        GateOutcome::GateError {
            reason,
            collector_exit_code,
            evidence_collected_at,
        } => {
            post_runtime_gate_error(
                reason,
                *collector_exit_code,
                *evidence_collected_at,
                resolved_mode,
                target,
                reporter,
                args,
                evidence_signer,
                activation_completed_at,
            )
            .await
        }
    }
}

async fn post_compliance_failures(
    failures: &[nixfleet_agent::compliance::ControlEvidence],
    evidence: &nixfleet_agent::compliance::ComplianceEvidence,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::warn!(
        count = failures.len(),
        "compliance gate: failures — posting per-control events",
    );
    for ctrl in failures {
        let articles =
            nixfleet_agent::compliance::flatten_framework_articles(&ctrl.framework_articles);
        let snippet = nixfleet_agent::compliance::truncate_evidence_snippet(&ctrl.checks);
        let snippet_sha =
            nixfleet_agent::evidence_signer::sha256_jcs(&snippet).unwrap_or_default();
        let signed_payload = nixfleet_agent::evidence_signer::ComplianceFailureSignedPayload {
            hostname: &args.machine_id,
            rollout: Some(&target.channel_ref),
            control_id: &ctrl.control,
            status: &ctrl.status,
            framework_articles: &articles,
            evidence_collected_at: evidence.timestamp,
            evidence_snippet_sha256: snippet_sha,
        };
        let signature = evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| s.sign(&signed_payload).ok());
        reporter
            .post_report(
                Some(&target.channel_ref),
                ReportEvent::ComplianceFailure {
                    control_id: ctrl.control.clone(),
                    status: ctrl.status.clone(),
                    framework_articles: articles,
                    evidence_snippet: Some(snippet),
                    evidence_collected_at: evidence.timestamp,
                    signature,
                },
            )
            .await;
    }
}

/// Post the gate-error event; if enforcing, also roll back and
/// post the rollback event. Returns true iff confirm must be
/// skipped (i.e. enforce mode triggered a rollback).
#[allow(clippy::too_many_arguments)]
async fn post_runtime_gate_error(
    reason: &str,
    collector_exit_code: Option<i32>,
    evidence_collected_at: Option<chrono::DateTime<chrono::Utc>>,
    resolved_mode: nixfleet_agent::compliance::GateMode,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
    activation_completed_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    use nixfleet_agent::compliance::GateMode;
    let enforcing = resolved_mode == GateMode::Enforce;
    if enforcing {
        tracing::error!(
            %reason,
            ?collector_exit_code,
            "compliance gate: ERROR — refusing confirm + rolling back (enforce mode)",
        );
    } else {
        tracing::warn!(
            %reason,
            ?collector_exit_code,
            "compliance gate: ERROR — posting event, allowing confirm (permissive mode)",
        );
    }
    let signed_payload = nixfleet_agent::evidence_signer::RuntimeGateErrorSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(&target.channel_ref),
        reason,
        collector_exit_code,
        evidence_collected_at,
        activation_completed_at,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&signed_payload).ok());
    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::RuntimeGateError {
                reason: reason.to_string(),
                collector_exit_code,
                evidence_collected_at,
                activation_completed_at,
                signature,
            },
        )
        .await;
    if enforcing {
        let _ = nixfleet_agent::activation::rollback().await;
        let rollback_reason = format!("compliance gate error: {reason}");
        let rollback_payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
            hostname: &args.machine_id,
            rollout: Some(&target.channel_ref),
            reason: &rollback_reason,
        };
        let rollback_signature = evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| s.sign(&rollback_payload).ok());
        reporter
            .post_report(
                Some(&target.channel_ref),
                ReportEvent::RollbackTriggered {
                    reason: rollback_reason,
                    signature: rollback_signature,
                },
            )
            .await;
    }
    enforcing
}

/// Confirm with the CP and persist the post-confirm bookkeeping.
/// CP-410 (cancelled / deadline-expired rollout) triggers a rollback.
async fn confirm_and_finalize(
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    client_handle: &reqwest::Client,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    let boot_id = nixfleet_agent::host_facts::boot_id().unwrap_or_else(|_| "unknown".to_string());
    let rollout = &target.channel_ref;
    // RFC-0003 §4.1: report the actual wave the agent activated in,
    // not a placeholder. CP populates `wave_index` at dispatch time
    // (control-plane/src/dispatch.rs); a None comes from older CPs
    // or channels with no wave plan, in which case 0 is the right
    // fallback (the dispatch already treats those as a single wave).
    let wave: u32 = target.wave_index.unwrap_or(0);
    match nixfleet_agent::activation::confirm_target(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        target,
        rollout,
        wave,
        &boot_id,
    )
    .await
    {
        Ok(nixfleet_agent::comms::ConfirmOutcome::Cancelled) => {
            handle_cp_cancellation(rollout, reporter, args, evidence_signer).await;
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Acknowledged) => {
            persist_confirmed_state(target, args);
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Other) => {}
        Err(err) => tracing::warn!(error = %err, "confirm post failed"),
    }
}

/// CP-driven rollback per `CheckinResponse.rollback`. Invoked when
/// the CP signals `on_health_failure = "rollback-and-halt"` for a
/// host that's reached the rollout's `Failed` state. Idempotent:
/// the agent's own rollback() is a no-op if already on the prior
/// gen, and the CP keeps re-emitting the signal until the agent's
/// `RollbackTriggered` post flips the host's state to `Reverted`.
pub(crate) async fn handle_cp_rollback_signal(
    rb: &nixfleet_proto::agent_wire::RollbackSignal,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::warn!(
        rollout = %rb.rollout,
        target_ref = %rb.target_ref,
        reason = %rb.reason,
        "agent: CP issued rollback signal (rollback-and-halt policy); rolling back",
    );
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let reason = rb.reason.clone();
    let rollback_payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(&rb.rollout),
        reason: &reason,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&rollback_payload).ok());
    reporter
        .post_report(
            Some(&rb.rollout),
            ReportEvent::RollbackTriggered { reason, signature },
        )
        .await;
    match &rb_outcome {
        Ok(o) if o.success() => {}
        Ok(o) => tracing::error!(
            phase = ?o.phase(),
            exit_code = ?o.exit_code(),
            "agent: CP-signalled rollback failed (poll/fire layer)",
        ),
        Err(err) => tracing::error!(
            error = %err,
            "agent: CP-signalled rollback transport-failed",
        ),
    }
}

async fn handle_cp_cancellation(
    rollout: &str,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let reason = "cp-410: rollout cancelled or deadline expired";
    let rollback_payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(rollout),
        reason,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&rollback_payload).ok());
    reporter
        .post_report(
            Some(rollout),
            ReportEvent::RollbackTriggered {
                reason: reason.to_string(),
                signature,
            },
        )
        .await;
    match &rb_outcome {
        Ok(o) if o.success() => {}
        Ok(o) => tracing::error!(
            phase = ?o.phase(),
            exit_code = ?o.exit_code(),
            "rollback after CP-410 failed (poll/fire layer)",
        ),
        Err(err) => tracing::error!(error = %err, "rollback after CP-410 transport-failed"),
    }
}

/// Best-effort: failure to persist doesn't roll back the activation.
/// `last_confirmed_at` feeds the CP's soak attestation on next checkin;
/// `last_dispatched` is cleared so a future agent restart's boot-recovery
/// path doesn't try to re-confirm an already-confirmed generation.
fn persist_confirmed_state(target: &nixfleet_proto::agent_wire::EvaluatedTarget, args: &Args) {
    if let Err(err) = nixfleet_agent::checkin_state::write_last_confirmed(
        &args.state_dir,
        &target.closure_hash,
        chrono::Utc::now(),
    ) {
        tracing::warn!(
            error = %err,
            state_dir = %args.state_dir.display(),
            "write_last_confirmed failed; soak attestation will be missing on next checkin",
        );
    }
    if let Err(err) = nixfleet_agent::checkin_state::clear_last_dispatched(&args.state_dir) {
        tracing::warn!(error = %err, "clear_last_dispatched failed (non-fatal)");
    }
}

async fn handle_realise_failed(
    reason: String,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::warn!(
        reason = %reason,
        "activation: realise failed; nothing switched, retrying next tick",
    );
    let payload = nixfleet_agent::evidence_signer::RealiseFailedSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(&target.channel_ref),
        closure_hash: &target.closure_hash,
        reason: &reason,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&payload).ok());
    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::RealiseFailed {
                closure_hash: target.closure_hash.clone(),
                reason,
                signature,
            },
        )
        .await;
}

async fn handle_signature_mismatch(
    closure_hash: String,
    stderr_tail: String,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::error!(
        closure_hash = %closure_hash,
        stderr_tail = %stderr_tail,
        "activation: closure signature mismatch — refused by nix substituter trust",
    );
    let stderr_tail_sha256 =
        nixfleet_agent::evidence_signer::sha256_jcs(&stderr_tail).unwrap_or_default();
    let payload = nixfleet_agent::evidence_signer::ClosureSignatureMismatchSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(&target.channel_ref),
        closure_hash: &closure_hash,
        stderr_tail_sha256,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&payload).ok());
    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::ClosureSignatureMismatch {
                closure_hash,
                stderr_tail,
                signature,
            },
        )
        .await;
}

async fn handle_switch_failed(
    phase: String,
    exit_code: Option<i32>,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::error!(phase = %phase, exit_code = ?exit_code, "activation: switch failed; rolling back");
    {
        let stderr_tail_sha256 =
            nixfleet_agent::evidence_signer::sha256_jcs(&"").unwrap_or_default();
        let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
            hostname: &args.machine_id,
            rollout: Some(&target.channel_ref),
            phase: &phase,
            exit_code,
            stderr_tail_sha256,
        };
        let signature = evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| s.sign(&payload).ok());
        reporter
            .post_report(
                Some(&target.channel_ref),
                ReportEvent::ActivationFailed {
                    phase: phase.clone(),
                    exit_code,
                    stderr_tail: None,
                    signature,
                },
            )
            .await;
    }
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let rollback_event = match &rb_outcome {
        Ok(o) if o.success() => {
            let reason = format!("activation phase {phase} failed");
            let payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                reason: &reason,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::RollbackTriggered { reason, signature }
        }
        Ok(o) => {
            let phase_str = format!(
                "rollback-after-{phase}/{}",
                o.phase().unwrap_or("unknown")
            );
            let exit = o.exit_code();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&"").unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                phase: &phase_str,
                exit_code: exit,
                stderr_tail_sha256,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: exit,
                stderr_tail: None,
                signature,
            }
        }
        Err(err) => {
            let phase_str = format!("rollback-after-{phase}");
            let stderr_tail = err.to_string();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&stderr_tail).unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                phase: &phase_str,
                exit_code: None,
                stderr_tail_sha256,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: None,
                stderr_tail: Some(stderr_tail),
                signature,
            }
        }
    };
    reporter
        .post_report(Some(&target.channel_ref), rollback_event)
        .await;
    if let Err(err) = rb_outcome {
        tracing::error!(
            error = %err,
            "rollback after failed switch also failed — manual intervention required",
        );
    }
}

/// Post-switch verify caught `/run/current-system` resolving to a
/// basename that is neither expected nor pre-switch. Emit a signed
/// `VerifyMismatch` then roll back, mirroring the failure-and-rollback
/// shape of `handle_switch_failed`.
async fn handle_verify_mismatch(
    expected: String,
    actual: String,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    tracing::error!(
        expected = %expected,
        actual = %actual,
        "activation: post-switch verify caught flip to unexpected closure; rolling back",
    );
    let payload = nixfleet_agent::evidence_signer::VerifyMismatchSignedPayload {
        hostname: &args.machine_id,
        rollout: Some(&target.channel_ref),
        expected: &expected,
        actual: &actual,
    };
    let signature = evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| s.sign(&payload).ok());
    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::VerifyMismatch {
                expected: expected.clone(),
                actual: actual.clone(),
                signature,
            },
        )
        .await;

    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let rollback_event = match &rb_outcome {
        Ok(o) if o.success() => {
            let reason = format!(
                "post-switch verify mismatch (expected {expected}, got {actual})"
            );
            let payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                reason: &reason,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::RollbackTriggered { reason, signature }
        }
        Ok(o) => {
            let phase_str = format!(
                "rollback-after-verify-mismatch/{}",
                o.phase().unwrap_or("unknown")
            );
            let exit = o.exit_code();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&"").unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                phase: &phase_str,
                exit_code: exit,
                stderr_tail_sha256,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: exit,
                stderr_tail: None,
                signature,
            }
        }
        Err(err) => {
            let phase_str = "rollback-after-verify-mismatch".to_string();
            let stderr_tail = err.to_string();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&stderr_tail).unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(&target.channel_ref),
                phase: &phase_str,
                exit_code: None,
                stderr_tail_sha256,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: None,
                stderr_tail: Some(stderr_tail),
                signature,
            }
        }
    };
    reporter
        .post_report(Some(&target.channel_ref), rollback_event)
        .await;
    if let Err(err) = rb_outcome {
        tracing::error!(
            error = %err,
            "rollback after verify mismatch also failed — manual intervention required",
        );
    }
}

/// Manifest gate failure (RFC-0002 §4.4): the CP advertised a
/// rolloutId we couldn't fetch, couldn't verify, or whose content
/// didn't match the partition-attack defenses. Emit the matching
/// signed `ReportEvent` and return — caller does not proceed with
/// any other field of `target`. No rollback because nothing was
/// activated.
async fn handle_manifest_error(
    err: nixfleet_agent::manifest_cache::ManifestError,
    rollout_id: &str,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    use nixfleet_agent::manifest_cache::ManifestError;
    let reason = err.reason().to_string();
    tracing::error!(
        rollout_id = %rollout_id,
        kind = match err {
            ManifestError::Missing(_) => "missing",
            ManifestError::VerifyFailed(_) => "verify-failed",
            ManifestError::Mismatch(_) => "mismatch",
        },
        reason = %reason,
        "agent: refusing dispatch — rollout manifest gate failed",
    );

    let event = match err {
        ManifestError::Missing(_) => {
            let payload = nixfleet_agent::evidence_signer::ManifestMissingSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(rollout_id),
                rollout_id,
                reason: &reason,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ManifestMissing {
                rollout_id: rollout_id.to_string(),
                reason,
                signature,
            }
        }
        ManifestError::VerifyFailed(_) => {
            let payload = nixfleet_agent::evidence_signer::ManifestVerifyFailedSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(rollout_id),
                rollout_id,
                reason: &reason,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ManifestVerifyFailed {
                rollout_id: rollout_id.to_string(),
                reason,
                signature,
            }
        }
        ManifestError::Mismatch(_) => {
            let payload = nixfleet_agent::evidence_signer::ManifestMismatchSignedPayload {
                hostname: &args.machine_id,
                rollout: Some(rollout_id),
                rollout_id,
                reason: &reason,
            };
            let signature = evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| s.sign(&payload).ok());
            ReportEvent::ManifestMismatch {
                rollout_id: rollout_id.to_string(),
                reason,
                signature,
            }
        }
    };

    reporter
        .post_report(Some(&target.channel_ref), event)
        .await;
}

/// Spawn / I/O error inside `activate `. State is unknown (could
/// have failed before realise even started) so we don't roll back.
async fn handle_activation_spawn_error(
    err: anyhow::Error,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    reporter: &impl Reporter,
) {
    tracing::error!(error = %err, "activation spawn failed");
    reporter
        .post_report(
            Some(&target.channel_ref),
            ReportEvent::Other {
                kind: "activation-spawn-failed".to_string(),
                detail: Some(serde_json::json!({
                    "error": err.to_string(),
                    "target_closure": target.closure_hash,
                })),
            },
        )
        .await;
}

#[cfg(test)]
mod tests {
    //! Per-variant unit tests for the dispatch handlers via a
    //! capturing `Reporter`. The handlers' branch logic + payload
    //! shape are covered here without ever talking to a real CP.
    //! End-to-end behavior (real activation, real switch poll) is
    //! exercised by the microvm harness on the lab.

    use super::*;
    use nixfleet_agent::comms::Reporter;
    use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Records every `post_report` call. Cheaply Clone-able (the
    /// inner Mutex is shared via Arc) so tests can hold one reference
    /// while the dispatch tree holds another.
    #[derive(Default)]
    struct FakeReporter {
        calls: Mutex<Vec<(Option<String>, ReportEvent)>>,
    }
    impl FakeReporter {
        fn new() -> Self {
            Self::default()
        }
        fn calls(&self) -> Vec<(Option<String>, ReportEvent)> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl Reporter for FakeReporter {
        async fn post_report(&self, rollout: Option<&str>, event: ReportEvent) {
            self.calls
                .lock()
                .unwrap()
                .push((rollout.map(String::from), event));
        }
    }

    fn sample_target() -> EvaluatedTarget {
        EvaluatedTarget {
            closure_hash: "abc123-test".to_string(),
            channel_ref: "stable@feedface".to_string(),
            evaluated_at: chrono::Utc::now(),
            rollout_id: None,
            wave_index: None,
            activate: None,
            signed_at: None,
            freshness_window_secs: None,
            compliance_mode: None,
        }
    }

    fn sample_args() -> Args {
        Args {
            control_plane_url: "https://cp.test".to_string(),
            machine_id: "test-host".to_string(),
            poll_interval: 60,
            trust_file: PathBuf::from("/dev/null"),
            ca_cert: None,
            client_cert: None,
            client_key: None,
            bootstrap_token_file: None,
            state_dir: PathBuf::from("/tmp/nixfleet-test"),
            compliance_gate_mode: None,
            ssh_host_key_file: PathBuf::from("/dev/null"),
        }
    }

    /// `handle_signature_mismatch` posts exactly one
    /// `ClosureSignatureMismatch` event with the supplied closure
    /// hash + stderr, and does NOT trigger a rollback (no rollback()
    /// shell-out, no follow-up `RollbackTriggered` event). The
    /// stderr is captured verbatim on the wire (truncation already
    /// happened upstream in `realise()`).
    #[tokio::test]
    async fn handle_signature_mismatch_posts_signed_event_and_does_not_attempt_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer = std::sync::Arc::new(None);

        handle_signature_mismatch(
            "abc123-bad-sig".to_string(),
            "error: lacks a valid signature".to_string(),
            &target,
            &fake,
            &args,
            &signer,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "expected exactly one post; got {:?}", calls);
        let (rollout, event) = &calls[0];
        assert_eq!(rollout.as_deref(), Some("stable@feedface"));
        match event {
            ReportEvent::ClosureSignatureMismatch {
                closure_hash,
                stderr_tail,
                signature,
            } => {
                assert_eq!(closure_hash, "abc123-bad-sig");
                assert_eq!(stderr_tail, "error: lacks a valid signature");
                assert!(
                    signature.is_none(),
                    "no evidence_signer wired → signature must be None",
                );
            }
            other => panic!("expected ClosureSignatureMismatch, got {other:?}"),
        }
    }

    /// `handle_realise_failed` produces exactly one `RealiseFailed`
    /// event with the failure reason, no rollback, no follow-up
    /// activation events.
    #[tokio::test]
    async fn handle_realise_failed_posts_one_event_no_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer = std::sync::Arc::new(None);

        handle_realise_failed(
            "network unreachable".to_string(),
            &target,
            &fake,
            &args,
            &signer,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0].1 {
            ReportEvent::RealiseFailed {
                closure_hash,
                reason,
                ..
            } => {
                assert_eq!(closure_hash, "abc123-test");
                assert_eq!(reason, "network unreachable");
            }
            other => panic!("expected RealiseFailed, got {other:?}"),
        }
    }
}
