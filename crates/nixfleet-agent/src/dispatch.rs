//! Dispatch path: `process_dispatch_target` + the `DispatchHandler`
//! family. The handler structs each consume a CP-issued failure
//! variant (one per `activation::ActivationOutcome` failure case,
//! plus the manifest-gate failures and the spawn-error catch-all)
//! and emit a signed `ReportEvent` via the [`Reporter`] trait —
//! optionally chaining into a local rollback + follow-up event.
//!
//! Lives as a binary-local module (`mod dispatch;` in main.rs)
//! because handlers depend on `super::Args` (the clap-parsed agent
//! CLI struct) for state-dir + compliance-mode. Side-effects route
//! through `&impl Reporter`, so handlers are unit-testable with a
//! capturing fake — see
//! `closure_signature_mismatch_handler_posts_signed_event_and_does_not_attempt_rollback`
//! in this file's tests.
//!
//! Adding a 7th failure variant is now a one-file change: declare
//! a new handler struct, impl `DispatchHandler`, route the matching
//! `ActivationOutcome` arm in `handle_activation_outcome`.
//!
//! Only two leaf functions still take a raw `&reqwest::Client`:
//! `process_dispatch_target` (manifest fetch) and
//! `confirm_and_finalize` (POST /v1/agent/confirm). Everything else
//! is reporter-only.

use std::future::Future;
use std::sync::Arc;

use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;

use super::Args;

/// Sign `payload` and surface signing failures at `error!` so the
/// silent-fail mode is observable in operator dashboards.
///
/// Returns `None` if the signer wasn't configured (operator opted out
/// of signed evidence) OR if signing failed at runtime. The two cases
/// are distinguished by the log: a configured-but-failed sign emits an
/// `error!` line with the cause; a not-configured signer emits
/// nothing. Without this helper the call sites used `.sign(...).ok()`
/// which collapsed the two cases into "just no signature" — auditors
/// reading the resulting unsigned event couldn't tell whether the
/// agent was meant to be signing and silently broke.
fn try_sign<T: serde::Serialize>(
    signer: &EvidenceSigner,
    payload: &T,
) -> Option<String> {
    match signer.sign(payload) {
        Ok(sig) => Some(sig),
        Err(err) => {
            tracing::error!(
                error = ?err,
                "evidence_signer.sign failed; posting unsigned event \
                 (signing was configured, runtime failure)",
            );
            None
        }
    }
}

/// Shared context for every `DispatchHandler` impl: the target being
/// acted on, the wire reporter, the agent CLI args (hostname,
/// state-dir, …) and the optional evidence signer.
pub(crate) struct DispatchCtx<'a, R: Reporter> {
    pub target: &'a EvaluatedTarget,
    pub reporter: &'a R,
    pub args: &'a Args,
    pub evidence_signer: &'a Arc<Option<EvidenceSigner>>,
}

/// Handler for a single dispatch-time failure. Each impl builds its
/// specific signed payload, posts via `ctx.reporter`, and decides
/// whether to call rollback + emit a follow-up event.
///
/// Telemetry-only: handlers never propagate errors. Anything that
/// could fail (rollback shell-out, reporter post) is logged and
/// dropped — the activation loop continues.
pub(crate) trait DispatchHandler {
    fn handle<R: Reporter>(
        &self,
        ctx: &DispatchCtx<'_, R>,
    ) -> impl Future<Output = ()> + Send;
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

/// Switch fired and polled successfully → run the runtime compliance
/// gate, then either confirm with the CP or roll back depending on
/// the gate outcome.
async fn handle_fired_and_polled<R: Reporter>(
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

/// Resolve the effective compliance mode (CP channel policy beats
/// the agent's CLI default) and run the runtime gate.
async fn run_runtime_gate(
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

/// Post events for the gate outcome; return true iff the agent
/// should skip confirm and stay on the rolled-back generation.
async fn process_gate_outcome<R: Reporter>(
    gate_outcome: &nixfleet_agent::compliance::GateOutcome,
    resolved_mode: nixfleet_agent::compliance::GateMode,
    ctx: &DispatchCtx<'_, R>,
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
            post_compliance_failures(failures, evidence, ctx).await;
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
                ctx,
                activation_completed_at,
            )
            .await
        }
    }
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

/// Post the gate-error event; if enforcing, also roll back and
/// post the rollback event. Returns true iff confirm must be
/// skipped (i.e. enforce mode triggered a rollback).
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
        let _ = nixfleet_agent::activation::rollback().await;
        let rollback_reason = format!("compliance gate error: {reason}");
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
    enforcing
}

/// Confirm with the CP and persist the post-confirm bookkeeping.
/// CP-410 (cancelled / deadline-expired rollout) triggers a rollback.
async fn confirm_and_finalize<R: Reporter>(
    ctx: &DispatchCtx<'_, R>,
    client_handle: &reqwest::Client,
) {
    let boot_id = nixfleet_agent::host_facts::boot_id().unwrap_or_else(|_| "unknown".to_string());
    let rollout = &ctx.target.channel_ref;
    // RFC-0003 §4.1: report the actual wave the agent activated in,
    // not a placeholder. CP populates `wave_index` at dispatch time
    // (control-plane/src/dispatch.rs); a None comes from older CPs
    // or channels with no wave plan, in which case 0 is the right
    // fallback (the dispatch already treats those as a single wave).
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

/// Best-effort: failure to persist doesn't roll back the activation.
/// `last_confirmed_at` feeds the CP's soak attestation on next checkin;
/// `last_dispatched` is cleared so a future agent restart's boot-recovery
/// path doesn't try to re-confirm an already-confirmed generation.
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
    if let Err(err) = nixfleet_agent::checkin_state::clear_last_dispatched(&args.state_dir) {
        tracing::warn!(error = %err, "clear_last_dispatched failed (non-fatal)");
    }
}

// =====================================================================
// DispatchHandler impls — one per dispatch-time failure variant.
// =====================================================================

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

/// Compose the follow-up `ReportEvent` posted after a `rollback()`
/// call. Shared by `SwitchFailedHandler` and `VerifyMismatchHandler`
/// — both run the same 3-arm match against `Result<RollbackOutcome>`,
/// differing only in the success-path reason string and the failure-
/// path phase prefix.
///
/// - `Ok(success)` → `RollbackTriggered { reason: success_reason, .. }`.
/// - `Ok(partial-fail)` → `ActivationFailed { phase: "{prefix}/{poll-phase}", exit_code, stderr_tail: None, .. }`.
/// - `Err(transport)` → `ActivationFailed { phase: prefix, exit_code: None, stderr_tail: Some(err), .. }`.
fn compose_rollback_followup_event<R: Reporter>(
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

/// Post-switch verify caught `/run/current-system` resolving to a
/// basename that is neither expected nor pre-switch. Emit a signed
/// `VerifyMismatch` then roll back, mirroring the failure-and-rollback
/// shape of `SwitchFailedHandler`.
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

/// Manifest gate failure (RFC-0002 §4.4): the CP advertised a
/// rolloutId we couldn't fetch, couldn't verify, or whose content
/// didn't match the partition-attack defenses. Emit the matching
/// signed `ReportEvent` and return — caller does not proceed with
/// any other field of `target`. No rollback because nothing was
/// activated.
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

/// Spawn / I/O error inside `activate`. State is unknown (could have
/// failed before realise even started) so we don't roll back. Posts
/// an unsigned `Other` event — the wire variant carries no signature
/// field, hence `ctx.args` / `ctx.evidence_signer` are unused here.
/// Kept under `DispatchHandler` for shape consistency with the other
/// 5 variants; if future signed-Other support lands the unused-field
/// note in this docblock goes away.
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

    fn ctx<'a, R: Reporter>(
        target: &'a EvaluatedTarget,
        reporter: &'a R,
        args: &'a Args,
        signer: &'a Arc<Option<EvidenceSigner>>,
    ) -> DispatchCtx<'a, R> {
        DispatchCtx {
            target,
            reporter,
            args,
            evidence_signer: signer,
        }
    }

    /// `ClosureSignatureMismatchHandler` posts exactly one
    /// `ClosureSignatureMismatch` event with the supplied closure
    /// hash + stderr, and does NOT trigger a rollback (no rollback()
    /// shell-out, no follow-up `RollbackTriggered` event). The
    /// stderr is captured verbatim on the wire (truncation already
    /// happened upstream in `realise()`).
    #[tokio::test]
    async fn closure_signature_mismatch_handler_posts_signed_event_and_does_not_attempt_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer: Arc<Option<EvidenceSigner>> = Arc::new(None);

        ClosureSignatureMismatchHandler {
            closure_hash: "abc123-bad-sig".to_string(),
            stderr_tail: "error: lacks a valid signature".to_string(),
        }
        .handle(&ctx(&target, &fake, &args, &signer))
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

    /// `RealiseFailedHandler` produces exactly one `RealiseFailed`
    /// event with the failure reason, no rollback, no follow-up
    /// activation events.
    #[tokio::test]
    async fn realise_failed_handler_posts_one_event_no_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer: Arc<Option<EvidenceSigner>> = Arc::new(None);

        RealiseFailedHandler {
            reason: "network unreachable".to_string(),
        }
        .handle(&ctx(&target, &fake, &args, &signer))
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
