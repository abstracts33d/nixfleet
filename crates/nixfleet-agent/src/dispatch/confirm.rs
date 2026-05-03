//! Success path: runtime gate → confirm → persist; CP-410 triggers rollback.

use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};

use nixfleet_agent::comms::Reporter;

use crate::Args;

use super::compliance::{process_gate_outcome, run_runtime_gate};
use nixfleet_agent::evidence_signer::try_sign;
use super::handler::DispatchCtx;

pub(super) async fn handle_fired_and_polled<R: Reporter>(
    ctx: &DispatchCtx<'_, R>,
    client_handle: &reqwest::Client,
) {
    let activation_completed_at = chrono::Utc::now();
    let (resolved_mode, gate_outcome) =
        run_runtime_gate(ctx.target, ctx.args, activation_completed_at).await;
    let gate_blocks_confirm =
        process_gate_outcome(&gate_outcome, resolved_mode, ctx, activation_completed_at).await;
    if gate_blocks_confirm {
        return;
    }
    confirm_and_finalize(ctx, client_handle).await;
}

async fn confirm_and_finalize<R: Reporter>(
    ctx: &DispatchCtx<'_, R>,
    client_handle: &reqwest::Client,
) {
    let boot_id = nixfleet_agent::host_facts::boot_id().unwrap_or_else(|_| "unknown".to_string());
    let rollout = &ctx.target.channel_ref;
    // wave_index None → 0: channels without an explicit wave plan (single-wave / coordinator).
    let wave: u32 = ctx.target.wave_index.unwrap_or(0);
    match nixfleet_agent::activation::confirm_target(
        client_handle,
        &ctx.args.control_plane_url,
        &ctx.args.machine_id,
        ctx.target,
        rollout,
        wave,
        &boot_id,
    )
    .await
    {
        Ok(nixfleet_agent::comms::ConfirmOutcome::Cancelled) => {
            handle_cp_cancellation(rollout, ctx).await;
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Acknowledged) => {
            persist_confirmed_state(ctx.target, ctx.args);
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Other) => {}
        Err(err) => tracing::warn!(error = %err, "confirm post failed"),
    }
}

async fn handle_cp_cancellation<R: Reporter>(rollout: &str, ctx: &DispatchCtx<'_, R>) {
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let reason = "cp-410: rollout cancelled or deadline expired";
    let rollback_payload = nixfleet_agent::evidence_signer::RollbackTriggeredSignedPayload {
        hostname: &ctx.args.machine_id,
        rollout: Some(rollout),
        reason,
    };
    let signature = ctx
        .evidence_signer
        .as_ref()
        .as_ref()
        .and_then(|s| try_sign(s, &rollback_payload));
    ctx.reporter
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

/// Persist failure is non-fatal; no rollback.
fn persist_confirmed_state(target: &EvaluatedTarget, args: &Args) {
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
    // LOADBEARING: outlives `clear_last_dispatched`. The CP's
    // outstanding-failure filter and active-rollouts panel both key off the
    // checkin's `last_evaluated_target.rollout_id`; without this breadcrumb
    // every event ever recorded looks "outstanding" forever.
    if let Err(err) = nixfleet_agent::checkin_state::write_last_target(&args.state_dir, target) {
        tracing::warn!(
            error = %err,
            "write_last_target failed; checkin will report no last_evaluated_target until next confirm",
        );
    }
    if let Err(err) = nixfleet_agent::checkin_state::clear_last_dispatched(&args.state_dir) {
        tracing::warn!(error = %err, "clear_last_dispatched failed (non-fatal)");
    }
}
