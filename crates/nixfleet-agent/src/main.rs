//! `nixfleet-agent` — Phase 3 PR-3 poll loop.
//!
//! Real main loop. Reads cert paths + CP URL from CLI flags, builds
//! an mTLS reqwest client, polls `/v1/agent/checkin` every
//! `pollInterval` seconds with a richer body than RFC-0003 §4.1's
//! minimum (pending generation, last-fetch outcome, agent uptime).
//! No activation — the response's `target` is logged but never
//! acted on (Phase 4 wires that).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use nixfleet_agent::{checkin_state, comms};
use nixfleet_proto::agent_wire::CheckinRequest;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-agent",
    version,
    about = "NixFleet v0.2 fleet agent (poll-only, Phase 3 PR-3)."
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
    // root for any in-process verification (PR-4 introduces the
    // direct-fetch fallback path that uses verify_artifact); for now
    // it's a contract-shape check.
    let trust_raw = std::fs::read_to_string(&args.trust_file).with_context(|| {
        format!("read trust file {}", args.trust_file.display())
    })?;
    let _trust: nixfleet_proto::TrustConfig =
        serde_json::from_str(&trust_raw).context("parse trust file")?;

    // PR-5: first-boot enrollment. When the agent starts and finds
    // no client cert at the configured path, AND a bootstrap token
    // is available, run /v1/enroll and write the issued cert + key
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

        // PR-5: self-paced renewal at 50% of cert validity. Each
        // tick checks the cert; if past 50%, generate a fresh CSR
        // and POST /v1/agent/renew via the current authenticated
        // client. Failure is non-fatal — next tick retries.
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
                    // Phase 4: realise + switch + verify the target.
                    // On full Success → POST /v1/agent/confirm. On any
                    // outcome that left the system in an unexpected
                    // state (SwitchFailed, VerifyMismatch) → local
                    // rollback. RealiseFailed left nothing switched —
                    // skip rollback, retry next tick. CP returning
                    // 410 from /confirm independently triggers rollback.
                    use nixfleet_agent::activation::ActivationOutcome;
                    match nixfleet_agent::activation::activate(target).await {
                        Ok(ActivationOutcome::Success) => {
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
                                    if let Err(err) =
                                        nixfleet_agent::activation::rollback().await
                                    {
                                        tracing::error!(
                                            error = %err,
                                            "rollback after CP-410 also failed",
                                        );
                                    }
                                }
                                Ok(_) => {} // Acknowledged or Other — done.
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
                        }
                        Ok(ActivationOutcome::SwitchFailed { exit_status }) => {
                            tracing::error!(
                                exit_code = ?exit_status.code(),
                                "activation: switch failed; rolling back",
                            );
                            if let Err(err) =
                                nixfleet_agent::activation::rollback().await
                            {
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
                            if let Err(err) =
                                nixfleet_agent::activation::rollback().await
                            {
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

    let req = CheckinRequest {
        hostname: args.machine_id.clone(),
        agent_version: AGENT_VERSION.to_string(),
        current_generation,
        pending_generation,
        last_evaluated_target: None,
        last_fetch_outcome: None,
        uptime_secs: Some(uptime_secs),
    };

    comms::checkin(client, &args.control_plane_url, &req).await
}
