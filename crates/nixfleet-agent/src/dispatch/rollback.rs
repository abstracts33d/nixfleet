//! CP-driven rollback per `CheckinResponse.rollback`. Invoked when
//! the CP signals `on_health_failure = "rollback-and-halt"` for a
//! host that's reached the rollout's `Failed` state. Idempotent: the
//! agent's own `rollback()` is a no-op if already on the prior gen,
//! and the CP keeps re-emitting the signal until the agent's
//! `RollbackTriggered` post flips the host's state to `Reverted`.

use nixfleet_proto::agent_wire::ReportEvent;

use nixfleet_agent::comms::Reporter;

use crate::Args;

use super::handler::try_sign;

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
        .and_then(|s| try_sign(s, &rollback_payload));
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
