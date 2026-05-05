//! `nixfleet` — top-level operator CLI. Subcommands implemented today:
//! `status`. Future surfaces (`rollout trace`, `diff`, `config init`)
//! per acceptance criteria of issue #66.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use nixfleet_cli::{render_status_table, render_trace_table, StatusInputs};
use nixfleet_proto::{HostsResponse, RolloutTrace};
use reqwest::{Certificate, Identity};

#[derive(Parser, Debug)]
#[command(name = "nixfleet", about = "NixFleet operator CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show fleet state: convergence, staleness, outstanding compliance per host.
    Status(StatusArgs),
    /// Rollout-scoped operations.
    #[command(subcommand)]
    Rollout(RolloutCommands),
}

#[derive(Subcommand, Debug)]
enum RolloutCommands {
    /// Wave-by-wave dispatch history for a rollout (dispatched_at + terminal_state).
    Trace(TraceArgs),
}

#[derive(clap::Args, Debug)]
struct TraceArgs {
    /// 64-char hex sha256 rollout id (matches `lookup` in /v1/rollouts).
    rollout_id: String,
    #[arg(long, env = "NIXFLEET_CP_URL")]
    cp_url: Option<String>,
    #[arg(long, env = "NIXFLEET_CA_CERT")]
    ca_cert: Option<PathBuf>,
    #[arg(long, env = "NIXFLEET_CLIENT_CERT")]
    client_cert: Option<PathBuf>,
    #[arg(long, env = "NIXFLEET_CLIENT_KEY")]
    client_key: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct StatusArgs {
    /// Control plane base URL (https://host:port).
    #[arg(long, env = "NIXFLEET_CP_URL")]
    cp_url: Option<String>,
    /// Path to the fleet CA cert (PEM).
    #[arg(long, env = "NIXFLEET_CA_CERT")]
    ca_cert: Option<PathBuf>,
    /// Operator client cert (PEM).
    #[arg(long, env = "NIXFLEET_CLIENT_CERT")]
    client_cert: Option<PathBuf>,
    /// Operator client key (PEM).
    #[arg(long, env = "NIXFLEET_CLIENT_KEY")]
    client_key: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Status(args) => run_status(args).await,
        Commands::Rollout(RolloutCommands::Trace(args)) => run_trace(args).await,
    }
}

async fn run_status(args: StatusArgs) -> Result<()> {
    let cp_url = args.cp_url.ok_or_else(|| {
        anyhow::anyhow!(
            "missing --cp-url (or NIXFLEET_CP_URL env). \
             Pass --cp-url + --ca-cert + --client-cert + --client-key, \
             or set the corresponding NIXFLEET_* env vars."
        )
    })?;
    let cp_url = cp_url.trim_end_matches('/').to_string();

    let client = build_client(
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    )?;

    let hosts: HostsResponse = client
        .get(format!("{cp_url}/v1/hosts"))
        .send()
        .await
        .with_context(|| format!("GET {cp_url}/v1/hosts"))?
        .error_for_status()?
        .json()
        .await
        .context("parse /v1/hosts response")?;

    let mut channels_seen: Vec<String> = hosts.hosts.iter().map(|h| h.channel.clone()).collect();
    channels_seen.sort();
    channels_seen.dedup();

    let mut channel_freshness: BTreeMap<String, u32> = BTreeMap::new();
    for channel in &channels_seen {
        let resp: serde_json::Value = client
            .get(format!("{cp_url}/v1/channels/{channel}"))
            .send()
            .await
            .with_context(|| format!("GET {cp_url}/v1/channels/{channel}"))?
            .error_for_status()?
            .json()
            .await
            .context("parse /v1/channels response")?;
        if let Some(window) = resp
            .get("freshness_window_minutes")
            .and_then(serde_json::Value::as_u64)
        {
            channel_freshness.insert(channel.clone(), window as u32);
        }
    }

    let inputs = StatusInputs {
        now: Utc::now(),
        hosts: hosts.hosts,
        channel_freshness,
    };
    print!("{}", render_status_table(&inputs));
    Ok(())
}

async fn run_trace(args: TraceArgs) -> Result<()> {
    let cp_url = args
        .cp_url
        .ok_or_else(|| anyhow::anyhow!("missing --cp-url (or NIXFLEET_CP_URL env)."))?;
    let cp_url = cp_url.trim_end_matches('/').to_string();
    let client = build_client(
        args.ca_cert.as_deref(),
        args.client_cert.as_deref(),
        args.client_key.as_deref(),
    )?;
    let url = format!("{cp_url}/v1/rollouts/{}/trace", args.rollout_id);
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "rollout {} has no dispatch history (never dispatched, or pruned past 90d retention)",
            args.rollout_id,
        );
    }
    let trace: RolloutTrace = resp
        .error_for_status()?
        .json()
        .await
        .context("parse /v1/rollouts/{id}/trace response")?;
    print!("{}", render_trace_table(&trace));
    Ok(())
}

/// Mirror of `nixfleet-agent::comms::build_client`. Kept inline (not
/// shared via a crate dep) to avoid pulling the agent's tokio + axum
/// transitive surface into the operator CLI.
fn build_client(
    ca_cert: Option<&Path>,
    client_cert: Option<&Path>,
    client_key: Option<&Path>,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().use_rustls_tls();
    if let Some(ca) = ca_cert {
        let pem =
            std::fs::read(ca).with_context(|| format!("read CA cert {}", ca.display()))?;
        let cert = Certificate::from_pem(&pem).context("parse CA cert PEM")?;
        builder = builder.add_root_certificate(cert);
    }
    if let (Some(cert), Some(key)) = (client_cert, client_key) {
        let mut pem = std::fs::read(cert)
            .with_context(|| format!("read client cert {}", cert.display()))?;
        let key_pem = std::fs::read(key)
            .with_context(|| format!("read client key {}", key.display()))?;
        pem.extend_from_slice(&key_pem);
        let identity = Identity::from_pem(&pem).context("parse client identity PEM")?;
        builder = builder.identity(identity);
    }
    builder.build().context("build HTTP client")
}
