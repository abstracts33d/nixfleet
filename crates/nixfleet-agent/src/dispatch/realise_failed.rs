//! Realise-step failure handlers: `RealiseFailed` (network /
//! substituter / missing-path) and `ClosureSignatureMismatch` (nix
//! refused the closure's narinfo signature). Neither rolls back —
//! nothing was switched.

use nixfleet_proto::agent_wire::ReportEvent;

use nixfleet_agent::comms::Reporter;

use super::handler::{try_sign, DispatchCtx, DispatchHandler};

pub(crate) struct RealiseFailedHandler {
    pub reason: String,
}

impl DispatchHandler for RealiseFailedHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        tracing::warn!(
            reason = %self.reason,
            "activation: realise failed; nothing switched, retrying next tick",
        );
        let payload = nixfleet_agent::evidence_signer::RealiseFailedSignedPayload {
            hostname: &ctx.args.machine_id,
            rollout: Some(&ctx.target.channel_ref),
            closure_hash: &ctx.target.closure_hash,
            reason: &self.reason,
        };
        let signature = ctx
            .evidence_signer
            .as_ref()
            .as_ref()
            .and_then(|s| try_sign(s, &payload));
        ctx.reporter
            .post_report(
                Some(&ctx.target.channel_ref),
                ReportEvent::RealiseFailed {
                    closure_hash: ctx.target.closure_hash.clone(),
                    reason: self.reason.clone(),
                    signature,
                },
            )
            .await;
    }
}

pub(crate) struct ClosureSignatureMismatchHandler {
    pub closure_hash: String,
    pub stderr_tail: String,
}

impl DispatchHandler for ClosureSignatureMismatchHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        tracing::error!(
            closure_hash = %self.closure_hash,
            stderr_tail = %self.stderr_tail,
            "activation: closure signature mismatch — refused by nix substituter trust",
        );
        let stderr_tail_sha256 =
            nixfleet_agent::evidence_signer::sha256_jcs(&self.stderr_tail).unwrap_or_default();
        let payload = nixfleet_agent::evidence_signer::ClosureSignatureMismatchSignedPayload {
            hostname: &ctx.args.machine_id,
            rollout: Some(&ctx.target.channel_ref),
            closure_hash: &self.closure_hash,
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
                ReportEvent::ClosureSignatureMismatch {
                    closure_hash: self.closure_hash.clone(),
                    stderr_tail: self.stderr_tail.clone(),
                    signature,
                },
            )
            .await;
    }
}
