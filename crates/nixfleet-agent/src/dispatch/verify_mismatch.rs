//! Switch / post-switch-verify failures: emit failure event, rollback, emit
//! follow-up event whose shape depends on the rollback outcome.

use nixfleet_proto::agent_wire::ReportEvent;

use nixfleet_agent::comms::Reporter;

use super::handler::{try_sign, DispatchCtx, DispatchHandler};

/// Shared by `SwitchFailedHandler` + `VerifyMismatchHandler`; arms map:
/// success → `RollbackTriggered`, partial-fail → `ActivationFailed{prefix/poll}`,
/// transport-err → `ActivationFailed{prefix, stderr_tail: err}`.
pub(super) fn compose_rollback_followup_event<R: Reporter>(
    rb_outcome: &anyhow::Result<nixfleet_agent::activation::RollbackOutcome>,
    ctx: &DispatchCtx<'_, R>,
    success_reason: String,
    failure_phase_prefix: &str,
) -> ReportEvent {
    match rb_outcome {
        Ok(o) if o.success() => {
            let payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
                hostname: &ctx.args.machine_id,
                rollout: Some(&ctx.target.channel_ref),
                reason: &success_reason,
            };
            let signature = ctx
                .evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| try_sign(s, &payload));
            ReportEvent::RollbackTriggered {
                reason: success_reason,
                signature,
            }
        }
        Ok(o) => {
            let phase_str = format!(
                "{failure_phase_prefix}/{}",
                o.phase().unwrap_or("unknown")
            );
            let exit = o.exit_code();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&"").unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &ctx.args.machine_id,
                rollout: Some(&ctx.target.channel_ref),
                phase: &phase_str,
                exit_code: exit,
                stderr_tail_sha256,
            };
            let signature = ctx
                .evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| try_sign(s, &payload));
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: exit,
                stderr_tail: None,
                signature,
            }
        }
        Err(err) => {
            let phase_str = failure_phase_prefix.to_string();
            let stderr_tail = err.to_string();
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&stderr_tail).unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &ctx.args.machine_id,
                rollout: Some(&ctx.target.channel_ref),
                phase: &phase_str,
                exit_code: None,
                stderr_tail_sha256,
            };
            let signature = ctx
                .evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| try_sign(s, &payload));
            ReportEvent::ActivationFailed {
                phase: phase_str,
                exit_code: None,
                stderr_tail: Some(stderr_tail),
                signature,
            }
        }
    }
}

pub(crate) struct SwitchFailedHandler {
    pub phase: String,
    pub exit_code: Option<i32>,
}

impl DispatchHandler for SwitchFailedHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        tracing::error!(
            phase = %self.phase,
            exit_code = ?self.exit_code,
            "activation: switch failed; rolling back",
        );
        {
            let stderr_tail_sha256 =
                nixfleet_agent::evidence_signer::sha256_jcs(&"").unwrap_or_default();
            let payload = nixfleet_agent::evidence_signer::ActivationFailedSignedPayload {
                hostname: &ctx.args.machine_id,
                rollout: Some(&ctx.target.channel_ref),
                phase: &self.phase,
                exit_code: self.exit_code,
                stderr_tail_sha256,
            };
            let signature = ctx
                .evidence_signer
                .as_ref()
                .as_ref()
                .and_then(|s| try_sign(s, &payload));
            ctx.reporter
                .post_report(
                    Some(&ctx.target.channel_ref),
                    ReportEvent::ActivationFailed {
                        phase: self.phase.clone(),
                        exit_code: self.exit_code,
                        stderr_tail: None,
                        signature,
                    },
                )
                .await;
        }
        let rb_outcome = nixfleet_agent::activation::rollback().await;
        let rollback_event = compose_rollback_followup_event(
            &rb_outcome,
            ctx,
            format!("activation phase {} failed", self.phase),
            &format!("rollback-after-{}", self.phase),
        );
        ctx.reporter
            .post_report(Some(&ctx.target.channel_ref), rollback_event)
            .await;
        if let Err(err) = rb_outcome {
            tracing::error!(
                error = %err,
                "rollback after failed switch also failed — manual intervention required",
            );
        }
    }
}

pub(crate) struct VerifyMismatchHandler {
    pub expected: String,
    pub actual: String,
}

impl DispatchHandler for VerifyMismatchHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        tracing::error!(
            expected = %self.expected,
            actual = %self.actual,
            "activation: post-switch verify caught flip to unexpected closure; rolling back",
        );
        let payload = nixfleet_agent::evidence_signer::VerifyMismatchSignedPayload {
            hostname: &ctx.args.machine_id,
            rollout: Some(&ctx.target.channel_ref),
            expected: &self.expected,
            actual: &self.actual,
        };
        let signature = ctx
            .evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| try_sign(s, &payload));
        ctx.reporter
            .post_report(
                Some(&ctx.target.channel_ref),
                ReportEvent::VerifyMismatch {
                    expected: self.expected.clone(),
                    actual: self.actual.clone(),
                    signature,
                },
            )
            .await;

        let rb_outcome = nixfleet_agent::activation::rollback().await;
        let rollback_event = compose_rollback_followup_event(
            &rb_outcome,
            ctx,
            format!(
                "post-switch verify mismatch (expected {}, got {})",
                self.expected, self.actual
            ),
            "rollback-after-verify-mismatch",
        );
        ctx.reporter
            .post_report(Some(&ctx.target.channel_ref), rollback_event)
            .await;
        if let Err(err) = rb_outcome {
            tracing::error!(
                error = %err,
                "rollback after verify mismatch also failed — manual intervention required",
            );
        }
    }
}
