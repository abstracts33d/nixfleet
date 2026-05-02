//! Manifest-gate failure handler (RFC-0002 §4.4 / RFC-0003 §4.1):
//! the CP advertised a `rolloutId` we couldn't fetch, couldn't
//! verify, or whose content didn't match the partition-attack
//! defenses. Emit the matching signed `ReportEvent` and return —
//! caller does not proceed with any other field of `target`. No
//! rollback because nothing was activated.

use nixfleet_proto::agent_wire::ReportEvent;

use nixfleet_agent::comms::Reporter;

use super::handler::{try_sign, DispatchCtx, DispatchHandler};

pub(crate) struct ManifestErrorHandler {
    pub err: nixfleet_agent::manifest_cache::ManifestError,
    pub rollout_id: String,
}

impl DispatchHandler for ManifestErrorHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        use nixfleet_agent::manifest_cache::ManifestError;
        let reason = self.err.reason().to_string();
        let kind = match self.err {
            ManifestError::Missing(_) => "missing",
            ManifestError::VerifyFailed(_) => "verify-failed",
            ManifestError::Mismatch(_) => "mismatch",
        };
        tracing::error!(
            rollout_id = %self.rollout_id,
            kind,
            reason = %reason,
            "agent: refusing dispatch — rollout manifest gate failed",
        );

        let rollout_id = self.rollout_id.as_str();
        let event = match self.err {
            ManifestError::Missing(_) => {
                let payload = nixfleet_agent::evidence_signer::ManifestMissingSignedPayload {
                    hostname: &ctx.args.machine_id,
                    rollout: Some(rollout_id),
                    rollout_id,
                    reason: &reason,
                };
                let signature = ctx
                    .evidence_signer
                    .as_ref()
                    .as_ref()
                    .and_then(|s| try_sign(s, &payload));
                ReportEvent::ManifestMissing {
                    rollout_id: rollout_id.to_string(),
                    reason,
                    signature,
                }
            }
            ManifestError::VerifyFailed(_) => {
                let payload = nixfleet_agent::evidence_signer::ManifestVerifyFailedSignedPayload {
                    hostname: &ctx.args.machine_id,
                    rollout: Some(rollout_id),
                    rollout_id,
                    reason: &reason,
                };
                let signature = ctx
                    .evidence_signer
                    .as_ref()
                    .as_ref()
                    .and_then(|s| try_sign(s, &payload));
                ReportEvent::ManifestVerifyFailed {
                    rollout_id: rollout_id.to_string(),
                    reason,
                    signature,
                }
            }
            ManifestError::Mismatch(_) => {
                let payload = nixfleet_agent::evidence_signer::ManifestMismatchSignedPayload {
                    hostname: &ctx.args.machine_id,
                    rollout: Some(rollout_id),
                    rollout_id,
                    reason: &reason,
                };
                let signature = ctx
                    .evidence_signer
                    .as_ref()
                    .as_ref()
                    .and_then(|s| try_sign(s, &payload));
                ReportEvent::ManifestMismatch {
                    rollout_id: rollout_id.to_string(),
                    reason,
                    signature,
                }
            }
        };

        ctx.reporter
            .post_report(Some(&ctx.target.channel_ref), event)
            .await;
    }
}
