//! Runtime compliance gate: resolve mode, run gate, post events; returns
//! whether confirm must be skipped (enforce-mode gate-error → rollback).

use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};

use nixfleet_agent::comms::Reporter;

use crate::Args;

use super::handler::{try_sign, DispatchCtx};

/// CP channel policy beats CLI default.
pub(super) async fn run_runtime_gate(
    target: &EvaluatedTarget,
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

/// Returns `true` iff the agent should skip confirm and stay rolled back.
pub(super) async fn process_gate_outcome<R: Reporter>(
    gate_outcome: &nixfleet_agent::compliance::GateOutcome,
    resolved_mode: nixfleet_agent::compliance::GateMode,
    ctx: &DispatchCtx<'_, R>,
    activation_completed_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    use nixfleet_agent::compliance::{GateMode, GateOutcome};
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
            post_compliance_failures(failures, evidence, ctx).await;
            // LOADBEARING: enforce mode must actually enforce. The per-control
            // events above are advisory; without rollback, the host stays on
            // a non-compliant closure and the docstring lie ("Failures block
            // dispatch / confirm and trigger recovery") becomes truth-by-fiat.
            if resolved_mode == GateMode::Enforce {
                let reason = compliance_failure_reason(failures);
                tracing::error!(
                    %reason,
                    failure_count = failures.len(),
                    "compliance gate: failures — refusing confirm + rolling back (enforce mode)",
                );
                trigger_rollback_with_reason(ctx, &reason).await;
                true
            } else {
                false
            }
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
                ctx,
                activation_completed_at,
            )
            .await
        }
    }
}

fn compliance_failure_reason(
    failures: &[nixfleet_agent::compliance::ControlEvidence],
) -> String {
    let ids: Vec<&str> = failures.iter().map(|c| c.control.as_str()).collect();
    format!("compliance failures: {}", ids.join(", "))
}

async fn post_compliance_failures<R: Reporter>(
    failures: &[nixfleet_agent::compliance::ControlEvidence],
    evidence: &nixfleet_agent::compliance::ComplianceEvidence,
    ctx: &DispatchCtx<'_, R>,
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
            hostname: &ctx.args.machine_id,
            rollout: Some(&ctx.target.channel_ref),
            control_id: &ctrl.control,
            status: &ctrl.status,
            framework_articles: &articles,
            evidence_collected_at: evidence.timestamp,
            evidence_snippet_sha256: snippet_sha,
        };
        let signature = ctx
            .evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| try_sign(s, &signed_payload));
        ctx.reporter
            .post_report(
                Some(&ctx.target.channel_ref),
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

/// Returns `true` iff enforce mode triggered a rollback.
async fn post_runtime_gate_error<R: Reporter>(
    reason: &str,
    collector_exit_code: Option<i32>,
    evidence_collected_at: Option<chrono::DateTime<chrono::Utc>>,
    resolved_mode: nixfleet_agent::compliance::GateMode,
    ctx: &DispatchCtx<'_, R>,
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
        hostname: &ctx.args.machine_id,
        rollout: Some(&ctx.target.channel_ref),
        reason,
        collector_exit_code,
        evidence_collected_at,
        activation_completed_at,
    };
    let signature = ctx
        .evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| try_sign(s, &signed_payload));
    ctx.reporter
        .post_report(
            Some(&ctx.target.channel_ref),
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
        trigger_rollback_with_reason(ctx, &format!("compliance gate error: {reason}")).await;
    }
    enforcing
}

/// Roll back to the prior generation, then report `RollbackTriggered`. The
/// reason in the report is qualified on rollback failure so the auditor chain
/// reflects whether the host actually returned to the previous closure.
async fn trigger_rollback_with_reason<R: Reporter>(
    ctx: &DispatchCtx<'_, R>,
    base_reason: &str,
) {
    let rollback_result = nixfleet_agent::activation::rollback().await;
    let rollback_reason = match &rollback_result {
        Ok(_) => base_reason.to_string(),
        Err(err) => {
            tracing::error!(
                error = %err,
                reason = %base_reason,
                "compliance gate: rollback FAILED — host left in inconsistent state",
            );
            format!("{base_reason}; rollback FAILED: {err}")
        }
    };
    let rollback_payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
        hostname: &ctx.args.machine_id,
        rollout: Some(&ctx.target.channel_ref),
        reason: &rollback_reason,
    };
    let rollback_signature = ctx
        .evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| try_sign(s, &rollback_payload));
    ctx.reporter
        .post_report(
            Some(&ctx.target.channel_ref),
            ReportEvent::RollbackTriggered {
                reason: rollback_reason,
                signature: rollback_signature,
            },
        )
        .await;
}
