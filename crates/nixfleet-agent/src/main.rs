#![allow(clippy::doc_lazy_continuation)]
//! `nixfleet-agent` — RFC-0009 §7.1 runtime entry point.
//!
//! Boot sequence:
//!   1. CLI parse + tracing init.
//!   2. First-boot enrollment (when no client cert yet).
//!   3. Trust-file sanity check.
//!   4. Boot-recovery handshake (runtime::recovery).
//!   5. Spawn the runtime (workers + reducer + outbound drainer).
//!   6. Wait for SIGINT / SIGTERM; cancel; drain.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio_util::sync::CancellationToken;

/// Background-task drain deadline on shutdown. Matches the systemd unit's
/// default `TimeoutStopSec`; staying under avoids SIGKILL.
const SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(25);

#[derive(Parser, Debug)]
#[command(name = "nixfleet-agent", version, about = "NixFleet fleet agent.")]
pub(crate) struct Args {
    #[arg(long, env = "NIXFLEET_AGENT_CP_URL")]
    pub(crate) control_plane_url: String,

    /// Must match the CN in the agent's client cert.
    #[arg(long, env = "NIXFLEET_AGENT_MACHINE_ID")]
    machine_id: String,

    #[arg(long, env = "NIXFLEET_AGENT_TRUST_FILE")]
    trust_file: PathBuf,

    #[arg(long, env = "NIXFLEET_AGENT_CA_CERT")]
    ca_cert: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_CERT")]
    client_cert: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_AGENT_CLIENT_KEY")]
    client_key: Option<PathBuf>,

    /// Acceptance window (seconds) for `meta.signedAt` on the
    /// fleet.resolved.json + per-rollout manifest verification (RFC-0005
    /// §1.5). Default `3600` matches the channel-refs poll cadence —
    /// any signed artifact older than one refresh cycle is rejected as
    /// stale (replay defense). Test harnesses with fixed-`signedAt`
    /// fixtures override to a much larger value (e.g. 1 year) to avoid
    /// regenerating signatures on every test run.
    #[arg(
        long,
        env = "NIXFLEET_AGENT_MANIFEST_FRESHNESS_WINDOW_SECS",
        default_value_t = nixfleet_agent::manifest_cache::DEFAULT_FRESHNESS_WINDOW_SECS
    )]
    manifest_freshness_window_secs: u64,

    /// When `client_cert` is absent and this is set, agent enrolls via /v1/enroll.
    #[arg(long, env = "NIXFLEET_AGENT_BOOTSTRAP_TOKEN_FILE")]
    bootstrap_token_file: Option<PathBuf>,

    #[arg(
        long,
        env = "NIXFLEET_AGENT_STATE_DIR",
        default_value = "/var/lib/nixfleet-agent"
    )]
    state_dir: PathBuf,

    /// Signs evidence payloads; absent file -> events post unsigned.
    /// Plumbed for v0.2 even though the runtime doesn't consume it yet;
    /// host-report signing reattaches to the runtime's outbound queue
    /// as a v0.2.1 follow-up.
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

    if let Err(err) = parse_trust_file(&args.trust_file) {
        tracing::error!(
            error = %format!("{err:#}"),
            "trust file parse failed (pre-cert); agent exiting"
        );
        return Err(err);
    }
    if let Err(err) = maybe_run_first_boot_enrollment(&args).await {
        tracing::error!(
            error = %format!("{err:#}"),
            "first-boot enrollment failed (pre-cert); agent exiting"
        );
        return Err(err);
    }

    // Boot-recovery handshake (RFC-0008 §9.5). Best-effort:
    // unreachable CP doesn't block startup; the steady-state heartbeat
    // worker keeps retrying.
    let clock: nixfleet_proto::clock::ClockHandle =
        std::sync::Arc::new(nixfleet_proto::clock::SystemClock::new());
    let recovery_outcome = nixfleet_agent::runtime::boot_recovery_handshake(
        &args.control_plane_url,
        &args.machine_id,
        &clock,
        &nixfleet_agent::runtime::recovery::default_current_system_path(),
        // mTLS paths threaded through (D-005); boot-recovery rides the
        // same enrollment-cert identity as the steady-state workers.
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    )
    .await;
    tracing::info!(
        machine_id = %args.machine_id,
        cp = %args.control_plane_url,
        current_closure = ?recovery_outcome.current_closure,
        replay_from = ?recovery_outcome.replay_from,
        bootstrap_count = recovery_outcome.bootstrap_rollouts.len(),
        "boot-recovery handshake complete; starting runtime",
    );

    // Spawn the runtime. The handle holds the reducer + all worker
    // JoinHandles; on shutdown we cancel + drain.
    let cancel = CancellationToken::new();
    let runtime = nixfleet_agent::runtime::spawn(
        cancel.clone(),
        nixfleet_agent::runtime::AgentConfig {
            control_plane_url: args.control_plane_url.clone(),
            machine_id: args.machine_id.clone(),
            state_dir: args.state_dir.clone(),
            trust_file: args.trust_file.clone(),
            manifest_freshness_window_secs: args.manifest_freshness_window_secs,
            // mTLS paths threaded through so workers can build an
            // authenticated client via `comms::build_client`. Same
            // three paths the enrollment path at
            // `maybe_run_first_boot_enrollment` already uses.
            ca_cert: args.ca_cert.clone(),
            client_cert: args.client_cert.clone(),
            client_key: args.client_key.clone(),
            current_system_path: nixfleet_agent::runtime::recovery::default_current_system_path(),
        },
        clock,
    );

    // LIFT #3: feed CP-supplied bootstrap snapshots through the reducer
    // BEFORE any worker can fire. The reducer consumes
    // `ReducerInput::BootstrapHost` synchronously off the MPSC; by the
    // time we return from these sends + the channel drains, the
    // reducer's in-memory HostRolloutState has the rehydrated records.
    // Subsequent worker ticks (probe runs, advance-ticker walks,
    // heartbeat round-trips) see populated state on first iteration.
    //
    // The MPSC is FIFO and the reducer is single-threaded; ordering
    // matches arrival. Backpressure: capacity is REDUCER_INPUT_CAPACITY
    // (8); typical bootstrap is ≤2 records per host. If a future fleet
    // exceeds that we revisit, but the bound is fine for v0.2.
    for snapshot in recovery_outcome.bootstrap_rollouts {
        if let Err(err) = runtime
            .input_tx
            .send(nixfleet_agent::runtime::ReducerInput::BootstrapHost(
                Box::new(snapshot),
            ))
            .await
        {
            tracing::warn!(
                target: "agent_recovery",
                error = %err,
                "bootstrap-snapshot send failed (reducer not ready?); skipping",
            );
        }
    }

    // Wait for SIGINT/SIGTERM, then drain.
    wait_for_shutdown_signal().await;
    tracing::info!(target: "shutdown", "shutdown signal received; cancelling runtime");
    cancel.cancel();

    let drain_fut = async {
        for handle in runtime.into_join_handles() {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = %err, "background task panicked during shutdown");
            }
        }
    };
    if tokio::time::timeout(SHUTDOWN_DRAIN_DEADLINE, drain_fut)
        .await
        .is_err()
    {
        tracing::warn!(
            target: "shutdown",
            deadline_secs = SHUTDOWN_DRAIN_DEADLINE.as_secs(),
            "background-task drain exceeded deadline; forcing exit",
        );
    }
    Ok(())
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
    tracing::info!(token = %token_file.display(), "no client cert - starting enrollment");
    let enroll_client = nixfleet_agent::comms::build_client(args.ca_cert.as_deref(), None, None)?;
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

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = sigint.recv() => {
            tracing::info!(target: "shutdown", "received SIGINT");
        }
        _ = sigterm.recv() => {
            tracing::info!(target: "shutdown", "received SIGTERM");
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
