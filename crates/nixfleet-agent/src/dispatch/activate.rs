//! Dispatch entry: freshness gate → manifest gate → activate → route outcome.

use std::sync::Arc;

use nixfleet_proto::agent_wire::{EvaluatedTarget, FetchOutcome, FetchResult, ReportEvent};
use nixfleet_proto::RolloutManifest;

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;
use nixfleet_agent::manifest_cache::ManifestError;

use crate::Args;

use super::confirm::handle_fired_and_polled;
use nixfleet_agent::evidence_signer::try_sign;
use super::handler::{DispatchCtx, DispatchHandler};
use super::manifest_error::ManifestErrorHandler;
use super::realise_failed::{ClosureSignatureMismatchHandler, RealiseFailedHandler};
use super::verify_mismatch::{SwitchFailedHandler, VerifyMismatchHandler};

/// Map a manifest-cache result onto the wire enum the CP circuit-breaker
/// understands. `Missing` is HTTP-shaped (404 / 5xx / network) → FetchFailed;
/// `VerifyFailed` and `Mismatch` are content-shaped → VerifyFailed.
fn fetch_outcome_for(result: &Result<RolloutManifest, ManifestError>) -> FetchOutcome {
    match result {
        Ok(_) => FetchOutcome {
            result: FetchResult::Ok,
            error: None,
        },
        Err(ManifestError::Missing(s)) => FetchOutcome {
            result: FetchResult::FetchFailed,
            error: Some(s.clone()),
        },
        Err(ManifestError::VerifyFailed(s)) | Err(ManifestError::Mismatch(s)) => FetchOutcome {
            result: FetchResult::VerifyFailed,
            error: Some(s.clone()),
        },
    }
}

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

    // LOADBEARING: verify manifest + membership BEFORE consuming any target field — refuse-to-act.
    if let Some(rollout_id) = target.rollout_id.as_deref() {
        let cache = nixfleet_agent::manifest_cache::ManifestCache::new(
            &args.state_dir,
            &args.trust_file,
        );
        let wave_index = target.wave_index.unwrap_or(0);
        let fetch_result = cache
            .ensure(client, &args.control_plane_url, rollout_id, &args.machine_id, wave_index)
            .await;
        // Persist outcome BEFORE any branch returns — CP's circuit breaker
        // (Decision::HoldAfterFailure) reads this on the next checkin.
        let _ = nixfleet_agent::checkin_state::write_last_fetch_outcome(
            &args.state_dir,
            &fetch_outcome_for(&fetch_result),
        );
        match fetch_result {
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

    // GOTCHA: write_last_dispatched failure only loses boot-recovery path — next-checkin re-dispatches.
    let dispatch_record = nixfleet_agent::checkin_state::LastDispatchRecord {
        closure_hash: target.closure_hash.clone(),
        channel_ref: target.channel_ref.clone(),
        rollout_id: target.rollout_id.clone(),
        compliance_mode: target.compliance_mode.clone(),
        confirm_endpoint: target
            .activate
            .as_ref()
            .map(|a| a.confirm_endpoint.clone()),
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

/// State unknown (may have failed pre-realise) so no rollback; posts unsigned `Other`.
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
