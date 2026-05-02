//! Dispatch entry: freshness gate, manifest gate, then `activate()`,
//! then route the result to the matching `DispatchHandler`.
//!
//! `process_dispatch_target` is the only `pub(crate)` symbol other
//! than the trait/ctx pair in `handler.rs`. `ActivationSpawnErrorHandler`
//! lives here because it's the catch-all when `activate()` itself
//! couldn't even spawn (state unknown — no rollback).

use std::sync::Arc;

use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;

use crate::Args;

use super::confirm::handle_fired_and_polled;
use super::handler::{try_sign, DispatchCtx, DispatchHandler};
use super::manifest_error::ManifestErrorHandler;
use super::realise_failed::{ClosureSignatureMismatchHandler, RealiseFailedHandler};
use super::verify_mismatch::{SwitchFailedHandler, VerifyMismatchHandler};

pub(crate) async fn process_dispatch_target(
    target: &EvaluatedTarget,
    reporter: &impl Reporter,
    client: &reqwest::Client,
    args: &Args,
    evidence_signer: &Arc<Option<EvidenceSigner>>,
) {
    let ctx = DispatchCtx {
        target,
        reporter,
        args,
        evidence_signer,
    };
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
                .and_then(|s| try_sign(s, &stale_payload));
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
                ManifestErrorHandler {
                    err,
                    rollout_id: rollout_id.to_string(),
                }
                .handle(&ctx)
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
    handle_activation_outcome(outcome, &ctx, client).await;
}

/// Dispatch on the result of `activation::activate`. Each failure
/// arm constructs the matching `DispatchHandler` impl and calls
/// `.handle(&ctx)`; the success arm runs the runtime compliance
/// gate + confirm path. Telemetry-only failures are logged, never
/// propagated.
async fn handle_activation_outcome<R: Reporter>(
    outcome: anyhow::Result<nixfleet_agent::activation::ActivationOutcome>,
    ctx: &DispatchCtx<'_, R>,
    client_handle: &reqwest::Client,
) {
    use nixfleet_agent::activation::ActivationOutcome;
    match outcome {
        Ok(ActivationOutcome::FiredAndPolled) => {
            handle_fired_and_polled(ctx, client_handle).await;
        }
        Ok(ActivationOutcome::RealiseFailed { reason }) => {
            RealiseFailedHandler { reason }.handle(ctx).await;
        }
        Ok(ActivationOutcome::SignatureMismatch {
            closure_hash,
            stderr_tail,
        }) => {
            ClosureSignatureMismatchHandler {
                closure_hash,
                stderr_tail,
            }
            .handle(ctx)
            .await;
        }
        Ok(ActivationOutcome::SwitchFailed { phase, exit_code }) => {
            SwitchFailedHandler { phase, exit_code }.handle(ctx).await;
        }
        Ok(ActivationOutcome::VerifyMismatch { expected, actual }) => {
            VerifyMismatchHandler { expected, actual }.handle(ctx).await;
        }
        Err(err) => {
            ActivationSpawnErrorHandler { err }.handle(ctx).await;
        }
    }
}

/// Spawn / I/O error inside `activate`. State is unknown (could have
/// failed before realise even started) so we don't roll back. Posts
/// an unsigned `Other` event — the wire variant carries no signature
/// field, hence `ctx.args` / `ctx.evidence_signer` are unused here.
pub(crate) struct ActivationSpawnErrorHandler {
    pub err: anyhow::Error,
}

impl DispatchHandler for ActivationSpawnErrorHandler {
    async fn handle<R: Reporter>(&self, ctx: &DispatchCtx<'_, R>) {
        tracing::error!(error = %self.err, "activation spawn failed");
        ctx.reporter
            .post_report(
                Some(&ctx.target.channel_ref),
                ReportEvent::Other {
                    kind: "activation-spawn-failed".to_string(),
                    detail: Some(serde_json::json!({
                        "error": self.err.to_string(),
                        "target_closure": ctx.target.closure_hash,
                    })),
                },
            )
            .await;
    }
}
