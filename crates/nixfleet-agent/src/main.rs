#![allow(clippy::doc_lazy_continuation)]
//! `nixfleet-agent` — main poll + activation loop.

mod dispatch;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use nixfleet_agent::{checkin_state, comms};
use nixfleet_proto::agent_wire::{CheckinRequest, ReportEvent};

use dispatch::{handle_cp_rollback_signal, process_dispatch_target};

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-agent",
    version,
    about = "NixFleet fleet agent."
)]
pub(crate) struct Args {
    #[arg(long, env = "NIXFLEET_AGENT_CP_URL")]
    pub(crate) control_plane_url: String,

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

    /// When `client_cert` is absent and this is set, agent enrolls via /v1/enroll.
    #[arg(long, env = "NIXFLEET_AGENT_BOOTSTRAP_TOKEN_FILE")]
    bootstrap_token_file: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_STATE_DIR", default_value = "/var/lib/nixfleet-agent")]
    state_dir: PathBuf,

    /// One of `"disabled"`, `"permissive"`, `"enforce"`, `"auto"`.
    /// CP-relayed channel mode wins when present.
    #[arg(long, env = "NIXFLEET_AGENT_COMPLIANCE_GATE_MODE")]
    compliance_gate_mode: Option<String>,

    /// Signs evidence payloads; absent file → events post unsigned.
    #[arg(
        long,
        env = "NIXFLEET_AGENT_SSH_HOST_KEY_FILE",
        default_value = "/etc/ssh/ssh_host_ed25519_key"
    )]
    ssh_host_key_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    let started_at = Instant::now();

    let evidence_signer = load_evidence_signer(&args.ssh_host_key_file);
    parse_trust_file(&args.trust_file)?;
    maybe_run_first_boot_enrollment(&args).await?;

    let client = comms::build_client(
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    )?;

    // Best-effort: next checkin re-converges via dispatch.
    let recovery_reporter = comms::ReqwestReporter::new(
        client.clone(),
        args.control_plane_url.clone(),
        args.machine_id.clone(),
        AGENT_VERSION,
    );
    if let Err(err) = check_boot_recovery(&client, &args, &recovery_reporter, &evidence_signer).await {
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

    run_poll_loop(client, &args, started_at, evidence_signer).await
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

/// Fail fast on misconfiguration; parsed value is otherwise unused.
fn parse_trust_file(path: &std::path::Path) -> anyhow::Result<()> {
    let trust_raw = std::fs::read_to_string(path)
        .with_context(|| format!("read trust file {}", path.display()))?;
    let _trust: nixfleet_proto::TrustConfig =
        serde_json::from_str(&trust_raw).context("parse trust file")?;
    Ok(())
}

/// Missing/unreadable key → events post unsigned. Hard-fail only on corrupt key.
fn load_evidence_signer(
    path: &std::path::Path,
) -> std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>> {
    let signer = match nixfleet_agent::evidence_signer::EvidenceSigner::load(path) {
        Ok(Some(s)) => {
            tracing::info!(
                path = %path.display(),
                "loaded SSH host key — evidence signing active",
            );
            Some(s)
        }
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %format!("{err:#}"),
                "ssh host key parse error — evidence signing disabled",
            );
            None
        }
    };
    std::sync::Arc::new(signer)
}

async fn maybe_run_first_boot_enrollment(args: &Args) -> anyhow::Result<()> {
    let (Some(cert_path), Some(key_path), Some(token_file)) = (
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
        args.bootstrap_token_file.as_deref(),
    ) else {
        return Ok(());
    };
    if cert_path.exists() {
        return Ok(());
    }
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
    .await
}

async fn run_poll_loop(
    client: reqwest::Client,
    args: &Args,
    started_at: Instant,
    evidence_signer: std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) -> anyhow::Result<()> {
    let mut ticker = tokio::time::interval(Duration::from_secs(args.poll_interval));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut client_handle = client;
    let mut reporter = comms::ReqwestReporter::new(
        client_handle.clone(),
        args.control_plane_url.clone(),
        args.machine_id.clone(),
        AGENT_VERSION,
    );
    // Exponential backoff with ±20% jitter; doubles per failure, capped at 8×.
    let mut consecutive_failures: u32 = 0;

    loop {
        if consecutive_failures > 0 {
            sleep_with_backoff(consecutive_failures, args.poll_interval).await;
        }
        ticker.tick().await;

        // LOADBEARING: retry boot-recovery every tick — startup POST races CP restart; missed confirm rolls back healthy host.
        if let Err(err) = check_boot_recovery(&client_handle, args, &reporter, &evidence_signer).await {
            tracing::warn!(
                error = %err,
                "boot-recovery retry (poll loop): non-fatal error; main loop continues",
            );
        }

        if let Some(new_client) = maybe_renew_cert(&client_handle, &reporter, args).await {
            client_handle = new_client;
            reporter.replace_client(client_handle.clone());
        }

        match send_checkin(&client_handle, args, started_at).await {
            Ok(resp) => {
                consecutive_failures = 0;
                // LOADBEARING: process CP rollback before new dispatch — host must step away from failed gen first.
                if let Some(rb) = &resp.rollback {
                    handle_cp_rollback_signal(rb, &reporter, args, &evidence_signer).await;
                }
                if let Some(target) = &resp.target {
                    process_dispatch_target(
                        target,
                        &reporter,
                        &client_handle,
                        args,
                        &evidence_signer,
                    )
                    .await;
                }
            }
            Err(err) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                // FOOTGUN: `{:#}` walks anyhow chain — `%err` alone hides TLS/connect cause below POST context.
                tracing::warn!(
                    error = %format!("{err:#}"),
                    consecutive_failures,
                    "checkin failed; will retry with backoff"
                );
            }
        }
    }
}

async fn sleep_with_backoff(consecutive_failures: u32, poll_interval: u64) {
    let multiplier = 1u64 << (consecutive_failures.min(3));
    let base = poll_interval.saturating_mul(multiplier);
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

/// Self-paced renewal at 50% of cert validity; returns the rebuilt client on success.
async fn maybe_renew_cert(
    client: &reqwest::Client,
    reporter: &impl comms::Reporter,
    args: &Args,
) -> Option<reqwest::Client> {
    let (Some(cert_path), Some(key_path)) =
        (args.client_cert.as_deref(), args.client_key.as_deref())
    else {
        return None;
    };
    let (remaining, _) =
        nixfleet_agent::enrollment::cert_remaining_fraction(cert_path, chrono::Utc::now()).ok()?;
    if remaining >= 0.5 {
        return None;
    }
    tracing::info!(remaining, "cert past 50% — renewing");
    if let Err(err) = nixfleet_agent::enrollment::renew(
        client,
        &args.control_plane_url,
        &args.machine_id,
        cert_path,
        key_path,
    )
    .await
    {
        tracing::warn!(error = %err, "renew failed; retry next tick");
        reporter
            .post_report(
                None,
                ReportEvent::RenewalFailed {
                    reason: err.to_string(),
                },
            )
            .await;
        return None;
    }
    match comms::build_client(
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    ) {
        Ok(new) => Some(new),
        Err(err) => {
            tracing::error!(error = %err, "rebuild client after renew");
            None
        }
    }
}

async fn send_checkin(
    client: &reqwest::Client,
    args: &Args,
    started_at: Instant,
) -> anyhow::Result<nixfleet_proto::agent_wire::CheckinResponse> {
    let current_generation = nixfleet_agent::host_facts::current_generation_ref()?;
    let pending_generation = nixfleet_agent::host_facts::pending_generation()?;
    let uptime_secs = checkin_state::uptime_secs(started_at);

    // read_last_confirmed returns Ok(None) on any mismatch (rollback, malformed,
    // future-dated) so the checkin always carries a sensible Option<DateTime>.
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

    let last_evaluated_target = match checkin_state::read_last_target(&args.state_dir) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                error = %err,
                state_dir = %args.state_dir.display(),
                "read_last_target failed; checkin proceeds without last_evaluated_target",
            );
            None
        }
    };

    let last_fetch_outcome = match checkin_state::read_last_fetch_outcome(&args.state_dir) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                error = %err,
                state_dir = %args.state_dir.display(),
                "read_last_fetch_outcome failed; checkin proceeds without it",
            );
            None
        }
    };

    let req = CheckinRequest {
        hostname: args.machine_id.clone(),
        agent_version: AGENT_VERSION.to_string(),
        current_generation,
        pending_generation,
        last_evaluated_target,
        last_fetch_outcome,
        uptime_secs: Some(uptime_secs),
        last_confirmed_at,
    };

    comms::checkin(client, &args.control_plane_url, &req).await
}

/// Closes the timing window where fire-and-forget activation self-kills
/// the agent mid-poll: matching dispatch record + live closure → retroactive confirm.
async fn check_boot_recovery(
    client: &reqwest::Client,
    args: &Args,
    reporter: &comms::ReqwestReporter,
    evidence_signer: &std::sync::Arc<Option<nixfleet_agent::evidence_signer::EvidenceSigner>>,
) -> anyhow::Result<()> {
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
        nixfleet_agent::recovery::GateInputs {
            reporter,
            evidence_signer,
            cli_default_mode: args.compliance_gate_mode.as_deref(),
        },
    )
    .await
}

