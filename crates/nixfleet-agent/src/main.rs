//! `nixfleet-agent` — main poll + activation loop.
//!
//! Reads cert paths + CP URL from CLI flags, builds an mTLS reqwest
//! client, polls `/v1/agent/checkin` every `pollInterval` seconds
//! with a richer body than RFC-0003 §4.1's minimum (pending
//! generation, last-fetch outcome, agent uptime). On a dispatched
//! target, realises and activates the closure, then confirms via
//! `/v1/agent/confirm`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use nixfleet_agent::{checkin_state, comms};
use nixfleet_proto::agent_wire::{CheckinRequest, ReportEvent, ReportRequest};

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build + POST a `/v1/agent/report` event to the CP. Best-effort:
/// telemetry MUST NOT crash the activation loop, so any HTTP / TLS
/// / serde failure is logged at warn and swallowed. The event is
/// already in the local journal (the caller logs first); the report
/// is purely for the operator's CP-side view.
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
    /// Control plane URL (e.g. https://lab:8080). Trailing slash
    /// optional.
    #[arg(long, env = "NIXFLEET_AGENT_CP_URL")]
    control_plane_url: String,

    /// This host's identifier — must match the CN in the agent
    /// client cert. Defaults to the system hostname when set by
    /// the NixOS module.
    #[arg(long, env = "NIXFLEET_AGENT_MACHINE_ID")]
    machine_id: String,

    /// Seconds between checkins. Default 60s, matching RFC-0003 §2
    /// and the CP's response `nextCheckinSecs`.
    #[arg(long, default_value_t = 60, env = "NIXFLEET_AGENT_POLL_INTERVAL")]
    poll_interval: u64,

    /// Path to trust.json. Read on startup; agent restarts on
    /// rebuild to pick up changes (docs/trust-root-flow.md §7.1).
    #[arg(long, env = "NIXFLEET_AGENT_TRUST_FILE")]
    trust_file: PathBuf,

    /// CA cert PEM for verifying the CP's TLS cert.
    #[arg(long, env = "NIXFLEET_AGENT_CA_CERT")]
    ca_cert: Option<PathBuf>,

    /// Client cert PEM (the agent's identity to the CP).
    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    /// Client private key PEM paired with `client_cert`.
    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_KEY")]
    client_key: Option<PathBuf>,

    /// Bootstrap token file. When `client_cert` doesn't exist on
    /// startup AND this is set, the agent enters first-boot
    /// enrollment: reads token, generates CSR, POSTs /v1/enroll,
    /// writes the issued cert + key to `client_cert` / `client_key`.
    #[arg(long, env = "NIXFLEET_AGENT_BOOTSTRAP_TOKEN_FILE")]
    bootstrap_token_file: Option<PathBuf>,

    /// Per-host state directory. The agent writes
    /// `last_confirmed_at` here on every successful confirm so it
    /// can attest the timestamp on every subsequent checkin
    /// (gap B in
    /// docs/roadmap/0002-v0.2-completeness-gaps.md). Survives agent
    /// process restart; CP-side `recover_soak_state_from_attestation`
    /// consumes the attestation to rebuild
    /// `host_rollout_state.last_healthy_since` after a CP rebuild.
    /// Default `/var/lib/nixfleet-agent`. The systemd unit
    /// (modules/scopes/nixfleet/_agent.nix or harness equivalent)
    /// sets `StateDirectory=` to this path.
    #[arg(long, env = "NIXFLEET_AGENT_STATE_DIR", default_value = "/var/lib/nixfleet-agent")]
    state_dir: PathBuf,
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

    // The trust file is parsed on startup just to fail fast if
    // misconfigured. The agent doesn't currently consume the trust
    // root for any in-process verification (a future direct-fetch
    // fallback path would use verify_artifact); for now it's a
    // contract-shape check.
    let trust_raw = std::fs::read_to_string(&args.trust_file).with_context(|| {
        format!("read trust file {}", args.trust_file.display())
    })?;
    let _trust: nixfleet_proto::TrustConfig =
        serde_json::from_str(&trust_raw).context("parse trust file")?;

    // First-boot enrollment. When the agent starts and finds no
    // client cert at the configured path, AND a bootstrap token is
    // available, run /v1/enroll and write the issued cert + key
    // before continuing to the poll loop.
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

    // ADR-011 boot recovery path. Runs once at startup, BEFORE the
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
    // RFC-0003 §5: exponential backoff with jitter on errors. Doubles
    // each consecutive failure, capped at 8× the base interval (~8min
    // at 60s default). Reset to 1× on first success. Random jitter
    // ±20% spreads recovery across the fleet.
    let mut consecutive_failures: u32 = 0;

    loop {
        // On consecutive failures, sleep extra (in addition to the
        // ticker's regular cadence). The ticker fires on its own
        // schedule; backoff is layered.
        if consecutive_failures > 0 {
            let multiplier = (1u64 << (consecutive_failures.min(3))) as u64; // 1, 2, 4, 8
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
                    // Issue #13 freshness gate: refuse to activate a
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

                    // Realise + switch + verify the target.
                    // On full FiredAndPolled → POST /v1/agent/confirm.
                    // On any outcome that left the system in an
                    // unexpected state (SwitchFailed, VerifyMismatch)
                    // → local rollback. RealiseFailed left nothing
                    // switched — skip rollback, retry next tick.
                    // CP returning 410 from /confirm independently
                    // triggers rollback.
                    use nixfleet_agent::activation::ActivationOutcome;
                    match nixfleet_agent::activation::activate(target).await {
                        Ok(ActivationOutcome::FiredAndPolled) => {
                            let boot_id = nixfleet_agent::checkin_state::boot_id()
                                .unwrap_or_else(|_| "unknown".to_string());
                            // Rollout id round-trips via target.channel_ref
                            // (CP populates it in the dispatch loop).
                            // Wave 0 — wave/soak staging is deferred.
                            let rollout = &target.channel_ref;
                            let wave: u32 = 0;
                            match nixfleet_agent::activation::confirm_target(
                                &client_handle,
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
                                    // CP says rollback — run it.
                                    let rb_outcome =
                                        nixfleet_agent::activation::rollback().await;
                                    post_report(
                                        &client_handle,
                                        &args.control_plane_url,
                                        &args.machine_id,
                                        Some(rollout),
                                        ReportEvent::RollbackTriggered {
                                            reason: "cp-410: rollout cancelled or deadline expired".to_string(),
                                        },
                                    )
                                    .await;
                                    if let Err(err) = rb_outcome {
                                        tracing::error!(
                                            error = %err,
                                            "rollback after CP-410 also failed",
                                        );
                                    }
                                }
                                Ok(nixfleet_agent::comms::ConfirmOutcome::Acknowledged) => {
                                    // Gap B: persist the confirm
                                    // timestamp so subsequent checkins
                                    // can attest it. Best-effort —
                                    // failure to persist doesn't roll
                                    // back the activation.
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
                                    // Confirm landed → the dispatch
                                    // record's job is done. Remove it
                                    // so a future agent restart's
                                    // boot-recovery path doesn't try
                                    // to re-confirm an already-confirmed
                                    // generation. Best-effort.
                                    if let Err(err) = nixfleet_agent::checkin_state::clear_last_dispatched(
                                        &args.state_dir,
                                    ) {
                                        tracing::warn!(
                                            error = %err,
                                            "clear_last_dispatched failed (non-fatal)",
                                        );
                                    }
                                }
                                Ok(nixfleet_agent::comms::ConfirmOutcome::Other) => {} // logged in confirm_target
                                Err(err) => {
                                    tracing::warn!(error = %err, "confirm post failed");
                                }
                            }
                        }
                        Ok(ActivationOutcome::RealiseFailed { reason }) => {
                            tracing::warn!(
                                reason = %reason,
                                "activation: realise failed; nothing switched, retrying next tick",
                            );
                            post_report(
                                &client_handle,
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
                        Ok(ActivationOutcome::SignatureMismatch {
                            closure_hash,
                            stderr_tail,
                        }) => {
                            tracing::error!(
                                closure_hash = %closure_hash,
                                stderr_tail = %stderr_tail,
                                "activation: closure signature mismatch — refused by nix substituter trust",
                            );
                            post_report(
                                &client_handle,
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
                        Ok(ActivationOutcome::SwitchFailed { phase, exit_code }) => {
                            tracing::error!(
                                phase = %phase,
                                exit_code = ?exit_code,
                                "activation: switch failed; rolling back",
                            );
                            post_report(
                                &client_handle,
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
                            let rb_outcome =
                                nixfleet_agent::activation::rollback().await;
                            let rollback_event = match &rb_outcome {
                                Ok(s) if s.success() => ReportEvent::RollbackTriggered {
                                    reason: format!("activation phase {phase} failed"),
                                },
                                Ok(s) => ReportEvent::ActivationFailed {
                                    phase: format!("rollback-after-{phase}"),
                                    exit_code: s.code(),
                                    stderr_tail: None,
                                },
                                Err(err) => ReportEvent::ActivationFailed {
                                    phase: format!("rollback-after-{phase}"),
                                    exit_code: None,
                                    stderr_tail: Some(err.to_string()),
                                },
                            };
                            post_report(
                                &client_handle,
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
                        Ok(ActivationOutcome::VerifyMismatch { expected, actual }) => {
                            tracing::error!(
                                expected = %expected,
                                actual = %actual,
                                "activation: post-switch verify mismatch; rolling back to defend against tampered closure",
                            );
                            post_report(
                                &client_handle,
                                &args.control_plane_url,
                                &args.machine_id,
                                Some(&target.channel_ref),
                                ReportEvent::VerifyMismatch {
                                    expected: expected.clone(),
                                    actual: actual.clone(),
                                },
                            )
                            .await;
                            let rb_outcome =
                                nixfleet_agent::activation::rollback().await;
                            post_report(
                                &client_handle,
                                &args.control_plane_url,
                                &args.machine_id,
                                Some(&target.channel_ref),
                                ReportEvent::RollbackTriggered {
                                    reason: format!(
                                        "post-switch verify mismatch (expected={expected}, actual={actual})"
                                    ),
                                },
                            )
                            .await;
                            if let Err(err) = rb_outcome {
                                tracing::error!(
                                    error = %err,
                                    "rollback after verify mismatch also failed — manual intervention required",
                                );
                            }
                        }
                        Err(err) => {
                            // Spawn / I/O error inside activate(). Don't
                            // roll back (state is unknown — could have
                            // failed before realise even started); log
                            // and let next tick retry.
                            tracing::error!(error = %err, "activation spawn failed");
                            post_report(
                                &client_handle,
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
                    }
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

async fn send_checkin(
    client: &reqwest::Client,
    args: &Args,
    started_at: Instant,
) -> anyhow::Result<nixfleet_proto::agent_wire::CheckinResponse> {
    let current_generation = checkin_state::current_generation_ref()?;
    let pending_generation = checkin_state::pending_generation()?;
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

/// ADR-011 boot recovery path. Closes the timing window where
/// fire-and-forget activation gets self-killed mid-poll.
///
/// Sequence:
///   1. Read `<state-dir>/last_dispatched`. Absent → no in-flight
///      dispatch from a prior agent run, nothing to recover.
///   2. Read `/run/current-system`. Compare basename to
///      `last_dispatched.closure_hash`.
///   3. **Match**: the prior agent fired a switch, got SIGTERMed by
///      the new closure's unit-restart, but `nixfleet-switch.service`
///      kept running and successfully activated the new closure.
///      Post the retroactive `/v1/agent/confirm`. On Acknowledged →
///      clear the dispatch record + write the confirm timestamp. On
///      410 → CP already deadline-rolled-back; we should rollback
///      locally too. On error → leave the record so a future cycle
///      can retry.
///   4. **Mismatch**: either we crashed before the switch took
///      effect (system stayed on old closure), or rollback fired and
///      we're back on the previous gen. Either way the dispatch
///      record describes a transient state the agent is no longer
///      in — clear it and let the next checkin re-decide.
///
/// All paths are best-effort: returns `Ok(())` on logical decisions
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

