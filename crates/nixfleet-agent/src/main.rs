#![allow(clippy::doc_lazy_continuation)]
//! `nixfleet-agent` — main poll + activation loop.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use nixfleet_agent::{checkin_state, comms};
use nixfleet_proto::agent_wire::{CheckinRequest, ReportEvent, ReportRequest};

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Best-effort: telemetry must never crash the activation loop.
/// Failures log at warn; the event is already in the local journal.
async fn post_report(
    client: &reqwest::Client,
    cp_url: &str,
    hostname: &str,
    rollout: Option<&str>,
    event: ReportEvent,
) {
    let req = ReportRequest {
        hostname: hostname.to_string(),
        agent_version: AGENT_VERSION.to_string(),
        occurred_at: chrono::Utc::now(),
        rollout: rollout.map(String::from),
        event,
    };
    if let Err(err) = comms::report(client, cp_url, &req).await {
        tracing::warn!(
            error = %err,
            hostname,
            "report post failed; event is in local journal only",
        );
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-agent",
    version,
    about = "NixFleet fleet agent."
)]
struct Args {
    #[arg(long, env = "NIXFLEET_AGENT_CP_URL")]
    control_plane_url: String,

    /// Must match the CN in the agent's client cert.
    #[arg(long, env = "NIXFLEET_AGENT_MACHINE_ID")]
    machine_id: String,

    #[arg(long, default_value_t = 60, env = "NIXFLEET_AGENT_POLL_INTERVAL")]
    poll_interval: u64,

    #[arg(long, env = "NIXFLEET_AGENT_TRUST_FILE")]
    trust_file: PathBuf,

    #[arg(long, env = "NIXFLEET_AGENT_CA_CERT")]
    ca_cert: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_KEY")]
    client_key: Option<PathBuf>,

    /// When `client_cert` doesn't exist on startup AND this is set,
    /// the agent enrolls via /v1/enroll and writes the issued cert
    /// to `client_cert` / `client_key`.
    #[arg(long, env = "NIXFLEET_AGENT_BOOTSTRAP_TOKEN_FILE")]
    bootstrap_token_file: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_STATE_DIR", default_value = "/var/lib/nixfleet-agent")]
    state_dir: PathBuf,

    /// One of `"disabled"`, `"permissive"`, `"enforce"`, `"auto"`.
    /// CP-relayed channel mode wins when present. `auto` resolves
    /// to `permissive` when `compliance-evidence-collector.service`
    /// is on this host, `disabled` when absent.
    #[arg(long, env = "NIXFLEET_AGENT_COMPLIANCE_GATE_MODE")]
    compliance_gate_mode: Option<String>,

    /// Used to sign `ComplianceFailure` / `RuntimeGateError`
    /// payloads. Missing file is fine — events post unsigned and
    /// are accepted but flagged unverified by the CP.
    #[arg(
        long,
        env = "NIXFLEET_AGENT_SSH_HOST_KEY_FILE",
        default_value = "/etc/ssh/ssh_host_ed25519_key"
    )]
    ssh_host_key_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let started_at = Instant::now();

    // Parsed on startup just to fail fast on misconfiguration.
    let trust_raw = std::fs::read_to_string(&args.trust_file).with_context(|| {
        format!("read trust file {}", args.trust_file.display())
    })?;
    let _trust: nixfleet_proto::TrustConfig =
        serde_json::from_str(&trust_raw).context("parse trust file")?;

    // Best-effort: missing/unreadable → signer is None → events
    // post unsigned. Hard-fail only on parse errors (corrupt key).
    let evidence_signer =
        match nixfleet_agent::evidence_signer::EvidenceSigner::load(&args.ssh_host_key_file) {
            Ok(Some(s)) => {
                tracing::info!(
                    path = %args.ssh_host_key_file.display(),
                    "loaded SSH host key — evidence signing active",
                );
                Some(s)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(
                    path = %args.ssh_host_key_file.display(),
                    error = %format!("{err:#}"),
                    "ssh host key parse error — evidence signing disabled",
                );
                None
            }
        };
    let evidence_signer = std::sync::Arc::new(evidence_signer);

    // First-boot enrollment when no client cert exists yet.
    if let (Some(cert_path), Some(key_path), Some(token_file)) = (
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
        args.bootstrap_token_file.as_deref(),
    ) {
        if !cert_path.exists() {
            tracing::info!(token = %token_file.display(), "no client cert — starting enrollment");
            let enroll_client = comms::build_client(args.ca_cert.as_deref(), None, None)?;
            nixfleet_agent::enrollment::enroll(
                &enroll_client,
                &args.control_plane_url,
                &args.machine_id,
                token_file,
                cert_path,
                key_path,
            )
            .await?;
        }
    }

    let client = comms::build_client(
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    )?;

    // boot recovery path. Runs once at startup, BEFORE the
    // poll loop. Detects the post-self-switch case where the agent
    // got SIGTERMed mid-fire-and-forget poll: the new closure
    // restarted nixfleet-agent.service, and on the new agent's boot
    // /run/current-system points at the closure we were trying to
    // dispatch. In that case, post the retroactive `/v1/agent/confirm`
    // so the CP doesn't run out the deadline and roll us back on a
    // success.
    //
    // Best-effort: failures are logged but not fatal. The next
    // regular checkin re-converges via dispatch decision either way.
    if let Err(err) = check_boot_recovery(&client, &args).await {
        tracing::warn!(
            error = %err,
            "boot-recovery path errored (non-fatal); main loop will re-converge",
        );
    }

    tracing::info!(
        machine_id = %args.machine_id,
        cp = %args.control_plane_url,
        interval_secs = args.poll_interval,
        "agent starting poll loop"
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(args.poll_interval));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut client_handle = client;
    // : exponential backoff with jitter on errors. Doubles
    // each consecutive failure, capped at 8× the base interval (~8min
    // at 60s default). Reset to 1× on first success. Random jitter
    // ±20% spreads recovery across the fleet.
    let mut consecutive_failures: u32 = 0;

    loop {
        // On consecutive failures, sleep extra (in addition to the
        // ticker's regular cadence). The ticker fires on its own
        // schedule; backoff is layered.
        if consecutive_failures > 0 {
            let multiplier = 1u64 << (consecutive_failures.min(3)); // 1, 2, 4, 8
            let base = args.poll_interval.saturating_mul(multiplier);
            // ±20% jitter.
            let jitter_pct: f64 = {
                use rand::Rng;
                rand::thread_rng().gen_range(-0.2_f64..=0.2_f64)
            };
            let jittered = (base as f64 * (1.0 + jitter_pct)) as u64;
            tracing::debug!(
                consecutive_failures,
                backoff_secs = jittered,
                "agent: backoff sleep"
            );
            tokio::time::sleep(Duration::from_secs(jittered)).await;
        }

        ticker.tick().await;

        // Self-paced renewal at 50% of cert validity. Each tick
        // checks the cert; if past 50%, generate a fresh CSR and
        // POST /v1/agent/renew via the current authenticated client.
        // Failure is non-fatal — next tick retries.
        if let (Some(cert_path), Some(key_path)) =
            (args.client_cert.as_deref(), args.client_key.as_deref())
        {
            if let Ok((remaining, _)) =
                nixfleet_agent::enrollment::cert_remaining_fraction(cert_path, chrono::Utc::now())
            {
                if remaining < 0.5 {
                    tracing::info!(remaining, "cert past 50% — renewing");
                    if let Err(err) = nixfleet_agent::enrollment::renew(
                        &client_handle,
                        &args.control_plane_url,
                        &args.machine_id,
                        cert_path,
                        key_path,
                    )
                    .await
                    {
                        tracing::warn!(error = %err, "renew failed; retry next tick");
                        post_report(
                            &client_handle,
                            &args.control_plane_url,
                            &args.machine_id,
                            None,
                            ReportEvent::RenewalFailed {
                                reason: err.to_string(),
                            },
                        )
                        .await;
                    } else {
                        // Rebuild the client with the new cert + key.
                        match comms::build_client(
                            args.ca_cert.as_deref(),
                            args.client_cert.as_deref(),
                            args.client_key.as_deref(),
                        ) {
                            Ok(new) => client_handle = new,
                            Err(err) => {
                                tracing::error!(error = %err, "rebuild client after renew");
                            }
                        }
                    }
                }
            }
        }

        match send_checkin(&client_handle, &args, started_at).await {
            Ok(resp) => {
                consecutive_failures = 0;
                if let Some(target) = &resp.target {
                    // freshness gate: refuse to activate a
                    // target whose backing fleet.resolved is older
                    // than the channel's freshness_window (with ±60s
                    // skew slack). Defense-in-depth — the CP applies
                    // the same gate at tick start, so a stale target
                    // reaching the agent normally points at a
                    // CP-side bug or clock-skew issue.
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
                            post_report(
                                &client_handle,
                                &args.control_plane_url,
                                &args.machine_id,
                                Some(&target.channel_ref),
                                ReportEvent::StaleTarget {
                                    closure_hash: target.closure_hash.clone(),
                                    channel_ref: target.channel_ref.clone(),
                                    signed_at,
                                    freshness_window_secs,
                                    age_secs,
                                },
                            )
                            .await;
                            continue;
                        }
                        FreshnessCheck::Unknown => {
                            tracing::debug!(
                                closure_hash = %target.closure_hash,
                                "agent: target lacks signed_at/freshness_window_secs — older CP, skipping freshness gate",
                            );
                        }
                        FreshnessCheck::Fresh => {}
                    }

                    // Persist the dispatch BEFORE firing so the
                    // post-self-switch boot-recovery path
                    // (`check_boot_recovery`) can detect "the new
                    // closure is live, send the retroactive confirm".
                    // Best-effort: failures log but do not block
                    // activation. The next regular checkin will
                    // re-dispatch if recovery can't find the record.
                    let dispatch_record = nixfleet_agent::checkin_state::LastDispatchRecord {
                        closure_hash: target.closure_hash.clone(),
                        channel_ref: target.channel_ref.clone(),
                        rollout_id: target.rollout_id.clone(),
                        dispatched_at: chrono::Utc::now(),
                    };
                    if let Err(err) = nixfleet_agent::checkin_state::write_last_dispatched(
                        &args.state_dir,
                        &dispatch_record,
                    ) {
                        tracing::warn!(
                            error = %err,
                            state_dir = %args.state_dir.display(),
                            "write_last_dispatched failed; boot-recovery path will fall back to next-checkin re-dispatch",
                        );
                    }

                    // pre-fire signal. Lets operators see
                    // "agent X started activating closure Y at
                    // timestamp T" in the CP report log, deterministic
                    // bookend to the post-poll success/failure event.
                    // Best-effort — failure to post doesn't block the
                    // activation itself.
                    post_report(
                        &client_handle,
                        &args.control_plane_url,
                        &args.machine_id,
                        Some(&target.channel_ref),
                        ReportEvent::ActivationStarted {
                            closure_hash: target.closure_hash.clone(),
                            channel_ref: target.channel_ref.clone(),
                        },
                    )
                    .await;

                    // Realise + switch + verify the target.
                    // On full FiredAndPolled → POST /v1/agent/confirm.
                    // On SwitchFailed (system in unexpected state) →
                    // local rollback. RealiseFailed left nothing
                    // switched — skip rollback, retry next tick.
                    // CP returning 410 from /confirm independently
                    // triggers rollback.
                    let outcome = nixfleet_agent::activation::activate(target).await;
                    handle_activation_outcome(
                        outcome,
                        target,
                        &client_handle,
                        &args,
                        &evidence_signer,
                    )
                    .await;
                }
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                tracing::warn!(
                    error = %err,
                    consecutive_failures,
                    "checkin failed; will retry with backoff"
                );
            }
        }
    }
}

/// Dispatch on the result of `activation::activate`. Telemetry-only
/// failures are logged, never propagated.
async fn handle_activation_outcome(
    outcome: anyhow::Result<nixfleet_agent::activation::ActivationOutcome>,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    client_handle: &reqwest::Client,
    args: &Args,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) {
    use nixfleet_agent::activation::ActivationOutcome;
    match outcome {
        Ok(ActivationOutcome::FiredAndPolled) => {
            handle_fired_and_polled(target, client_handle, args, evidence_signer).await;
        }
        Ok(ActivationOutcome::RealiseFailed { reason }) => {
            handle_realise_failed(reason, target, client_handle, args).await;
        }
        Ok(ActivationOutcome::SignatureMismatch {
            closure_hash,
            stderr_tail,
        }) => {
            handle_signature_mismatch(closure_hash, stderr_tail, target, client_handle, args).await;
        }
        Ok(ActivationOutcome::SwitchFailed { phase, exit_code }) => {
            handle_switch_failed(phase, exit_code, target, client_handle, args).await;
        }
        Err(err) => {
            handle_activation_spawn_error(err, target, client_handle, args).await;
        }
    }
}

/// Switch fired and polled successfully → run the runtime compliance
/// gate, then either confirm with the CP or roll back depending on
/// the gate outcome.
async fn handle_fired_and_polled(
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
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
        client_handle,
        args,
        evidence_signer,
        activation_completed_at,
    )
    .await;
    if gate_blocks_confirm {
        return;
    }
    confirm_and_finalize(target, client_handle, args).await;
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
    client_handle: &reqwest::Client,
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
            post_compliance_failures(failures, evidence, target, client_handle, args, evidence_signer)
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
                client_handle,
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
    client_handle: &reqwest::Client,
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
        post_report(
            client_handle,
            &args.control_plane_url,
            &args.machine_id,
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
    client_handle: &reqwest::Client,
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
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
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
        post_report(
            client_handle,
            &args.control_plane_url,
            &args.machine_id,
            Some(&target.channel_ref),
            ReportEvent::RollbackTriggered {
                reason: format!("compliance gate error: {reason}"),
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
    client_handle: &reqwest::Client,
    args: &Args,
) {
    use nixfleet_agent::host_facts::{Host, HostFacts};
    let boot_id = Host::new()
        .boot_id()
        .unwrap_or_else(|_| "unknown".to_string());
    let rollout = &target.channel_ref;
    // Wave 0 — wave/soak staging is deferred.
    let wave: u32 = 0;
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
            handle_cp_cancellation(rollout, client_handle, args).await;
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Acknowledged) => {
            persist_confirmed_state(target, args);
        }
        Ok(nixfleet_agent::comms::ConfirmOutcome::Other) => {}
        Err(err) => tracing::warn!(error = %err, "confirm post failed"),
    }
}

async fn handle_cp_cancellation(rollout: &str, client_handle: &reqwest::Client, args: &Args) {
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        Some(rollout),
        ReportEvent::RollbackTriggered {
            reason: "cp-410: rollout cancelled or deadline expired".to_string(),
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
    client_handle: &reqwest::Client,
    args: &Args,
) {
    tracing::warn!(
        reason = %reason,
        "activation: realise failed; nothing switched, retrying next tick",
    );
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        Some(&target.channel_ref),
        ReportEvent::RealiseFailed {
            closure_hash: target.closure_hash.clone(),
            reason,
        },
    )
    .await;
}

async fn handle_signature_mismatch(
    closure_hash: String,
    stderr_tail: String,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    client_handle: &reqwest::Client,
    args: &Args,
) {
    tracing::error!(
        closure_hash = %closure_hash,
        stderr_tail = %stderr_tail,
        "activation: closure signature mismatch — refused by nix substituter trust",
    );
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        Some(&target.channel_ref),
        ReportEvent::ClosureSignatureMismatch {
            closure_hash,
            stderr_tail,
        },
    )
    .await;
}

async fn handle_switch_failed(
    phase: String,
    exit_code: Option<i32>,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    client_handle: &reqwest::Client,
    args: &Args,
) {
    tracing::error!(phase = %phase, exit_code = ?exit_code, "activation: switch failed; rolling back");
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        Some(&target.channel_ref),
        ReportEvent::ActivationFailed {
            phase: phase.clone(),
            exit_code,
            stderr_tail: None,
        },
    )
    .await;
    let rb_outcome = nixfleet_agent::activation::rollback().await;
    let rollback_event = match &rb_outcome {
        Ok(o) if o.success() => ReportEvent::RollbackTriggered {
            reason: format!("activation phase {phase} failed"),
        },
        Ok(o) => ReportEvent::ActivationFailed {
            phase: format!(
                "rollback-after-{phase}/{}",
                o.phase().unwrap_or("unknown")
            ),
            exit_code: o.exit_code(),
            stderr_tail: None,
        },
        Err(err) => ReportEvent::ActivationFailed {
            phase: format!("rollback-after-{phase}"),
            exit_code: None,
            stderr_tail: Some(err.to_string()),
        },
    };
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
        Some(&target.channel_ref),
        rollback_event,
    )
    .await;
    if let Err(err) = rb_outcome {
        tracing::error!(
            error = %err,
            "rollback after failed switch also failed — manual intervention required",
        );
    }
}

/// Spawn / I/O error inside `activate `. State is unknown (could
/// have failed before realise even started) so we don't roll back.
async fn handle_activation_spawn_error(
    err: anyhow::Error,
    target: &nixfleet_proto::agent_wire::EvaluatedTarget,
    client_handle: &reqwest::Client,
    args: &Args,
) {
    tracing::error!(error = %err, "activation spawn failed");
    post_report(
        client_handle,
        &args.control_plane_url,
        &args.machine_id,
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

async fn send_checkin(
    client: &reqwest::Client,
    args: &Args,
    started_at: Instant,
) -> anyhow::Result<nixfleet_proto::agent_wire::CheckinResponse> {
    use nixfleet_agent::host_facts::{Host, HostFacts};
    let host = Host::new();
    let current_generation = host.current_generation_ref()?;
    let pending_generation = host.pending_generation()?;
    let uptime_secs = checkin_state::uptime_secs(started_at);

    // Gap B: attest the most recent confirm timestamp when it
    // applies to the live closure. read_last_confirmed handles all
    // mismatch cases (rolled-back closure, missing file, malformed,
    // future-dated) by returning Ok(None) so the checkin always
    // populates a sensible Option<DateTime>.
    let last_confirmed_at = match checkin_state::read_last_confirmed(
        &args.state_dir,
        &current_generation.closure_hash,
        chrono::Utc::now(),
    ) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                error = %err,
                state_dir = %args.state_dir.display(),
                "read_last_confirmed failed; checkin proceeds without attestation",
            );
            None
        }
    };

    let req = CheckinRequest {
        hostname: args.machine_id.clone(),
        agent_version: AGENT_VERSION.to_string(),
        current_generation,
        pending_generation,
        last_evaluated_target: None,
        last_fetch_outcome: None,
        uptime_secs: Some(uptime_secs),
        last_confirmed_at,
    };

    comms::checkin(client, &args.control_plane_url, &req).await
}

/// boot recovery path. Closes the timing window where
/// fire-and-forget activation gets self-killed mid-poll.
///
/// Sequence:
/// 1. Read `<state-dir>/last_dispatched`. Absent → no in-flight
/// dispatch from a prior agent run, nothing to recover.
/// 2. Read `/run/current-system`. Compare basename to
/// `last_dispatched.closure_hash`.
/// 3. **Match**: the prior agent fired a switch, got SIGTERMed by
/// the new closure's unit-restart, but `nixfleet-switch.service`
/// kept running and successfully activated the new closure.
/// Post the retroactive `/v1/agent/confirm`. On Acknowledged →
/// clear the dispatch record + write the confirm timestamp. On
/// 410 → CP already deadline-rolled-back; we should rollback
/// locally too. On error → leave the record so a future cycle
/// can retry.
/// 4. **Mismatch**: either we crashed before the switch took
/// effect (system stayed on old closure), or rollback fired and
/// we're back on the previous gen. Either way the dispatch
/// record describes a transient state the agent is no longer
/// in — clear it and let the next checkin re-decide.
///
/// All paths are best-effort: returns `Ok( )` on logical decisions
/// (mismatch, no-record, post-failure-but-not-a-bug); `Err` only on
/// genuinely-unexpected I/O failures. The main loop's normal poll
/// cadence is the safety net — even total recovery failure means
/// the agent eventually re-dispatches and converges.
async fn check_boot_recovery(client: &reqwest::Client, args: &Args) -> anyhow::Result<()> {
    let current = match checkin_state::current_closure_hash() {
        Ok(c) => Some(c),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "boot-recovery: cannot read /run/current-system; skipping recovery this boot",
            );
            None
        }
    };
    nixfleet_agent::recovery::run_boot_recovery(
        client,
        &args.state_dir,
        &args.control_plane_url,
        &args.machine_id,
        current,
    )
    .await
}

