//! `nixfleet-control-plane` — CLI shell.
//!
//! Two subcommands:
//!
//! * `serve` (default) — long-running TLS server. axum + tokio +
//!   axum-server. Internal 30s reconcile loop. Exposes `GET /healthz`
//!   and the `/v1/*` agent endpoints.
//!
//! * `tick` — oneshot: read inputs, verify, reconcile, print plan,
//!   exit. Preserved for tests + ad-hoc operator runs (handy for
//!   diffing what the loop is doing without tailing journald).
//!
//! Exit codes for `tick`:
//! - 0 — verify ok, plan emitted (the plan may be empty — no drift).
//! - 1 — verify failed; one summary line emitted with the reason.
//! - 2 — input/IO/parse error before verify could run.
//!
//! `serve` runs until interrupted; exit code 0 on graceful shutdown,
//! non-zero if startup (cert load, port bind) fails.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use chrono::Utc;
use clap::{Parser, Subcommand};
use nixfleet_control_plane::{render_plan, server, tick, TickInputs, VerifyOutcome};

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-control-plane",
    version,
    about = "NixFleet control plane: long-running TLS server + reconciler."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Long-running TLS server with internal reconcile loop. The
    /// natural operator default — `nixfleet-control-plane serve`.
    Serve(ServeFlags),
    /// One-shot tick: read inputs, verify, reconcile, print, exit.
    /// For tests + ad-hoc operator runs (handy for diffing what the
    /// loop is doing without tailing journald).
    Tick(TickFlags),
}

#[derive(Parser, Debug, Clone)]
struct ServeFlags {
    /// Address to listen on (HOST:PORT).
    #[arg(long, default_value = "0.0.0.0:8080", env = "NIXFLEET_CP_LISTEN")]
    listen: String,

    /// TLS server certificate PEM file.
    #[arg(long, env = "NIXFLEET_CP_TLS_CERT")]
    tls_cert: PathBuf,

    /// TLS server private key PEM file.
    #[arg(long, env = "NIXFLEET_CP_TLS_KEY")]
    tls_key: PathBuf,

    /// Client CA PEM file. When set, server requires verified client
    /// certs (mTLS). Optional; the standard deploy sets it.
    #[arg(long, env = "NIXFLEET_CP_CLIENT_CA")]
    client_ca: Option<PathBuf>,

    /// Path to releases/fleet.resolved.json (the bytes CI signed).
    #[arg(long)]
    artifact: PathBuf,

    /// Path to releases/fleet.resolved.json.sig.
    #[arg(long)]
    signature: PathBuf,

    /// Path to trust.json (shape per docs/trust-root-flow.md §3.4).
    #[arg(long, default_value = "/etc/nixfleet/cp/trust.json")]
    trust_file: PathBuf,

    /// Path to observed state JSON (shape per
    /// `nixfleet_reconciler::Observed`). The in-memory projection
    /// from agent check-ins is preferred; the flag remains as a
    /// dev/test fallback.
    #[arg(long)]
    observed: PathBuf,

    /// Maximum age (seconds) of meta.signedAt relative to now.
    #[arg(long, default_value_t = 86400)]
    freshness_window_secs: u64,

    // Channel-refs poll. The artifact + signature URLs together gate
    // whether the poll task is spawned — set both to enable, leave
    // both unset to fall back to file-backed channel-refs from
    // observed.json. Source-agnostic: any HTTP(S) URL that yields the
    // raw bytes of the artifact / signature works (Forgejo `raw`
    // path, GitHub `raw.githubusercontent.com`, GitLab `/-/raw/...`,
    // a plain file server, etc.). Concrete URL templates for common
    // forges live in `nixfleet-scopes/modules/scopes/gitops/`.
    /// URL that yields the raw bytes of the canonical signed
    /// fleet.resolved.json. When unset, channel-refs polling is
    /// disabled.
    #[arg(long, env = "NIXFLEET_CP_CHANNEL_REFS_ARTIFACT_URL")]
    channel_refs_artifact_url: Option<String>,

    /// URL that yields the raw bytes of the matching signature. When
    /// unset, channel-refs polling is disabled.
    #[arg(long, env = "NIXFLEET_CP_CHANNEL_REFS_SIGNATURE_URL")]
    channel_refs_signature_url: Option<String>,

    /// Path to a file containing the upstream API token (sent as
    /// `Authorization: Bearer <token>`). Optional — leave unset for
    /// public sources. Read on each poll so token rotation
    /// propagates without restart.
    #[arg(long, env = "NIXFLEET_CP_CHANNEL_REFS_TOKEN_FILE")]
    channel_refs_token_file: Option<PathBuf>,

    // Cert issuance (enroll + renew). The CP holds the fleet CA
    // private key online — see issue #41 for the deferred TPM-bound
    // replacement. When these are unset, /v1/enroll and
    // /v1/agent/renew return 500.
    /// Fleet CA cert path (read on each issuance for the chain).
    #[arg(long, env = "NIXFLEET_CP_FLEET_CA_CERT")]
    fleet_ca_cert: Option<PathBuf>,

    /// Fleet CA private key path (used to sign agent certs).
    #[arg(long, env = "NIXFLEET_CP_FLEET_CA_KEY")]
    fleet_ca_key: Option<PathBuf>,

    /// Audit log path. JSON-lines, one record per issuance (enroll
    /// or renew). Best-effort writes; failure logs a warn but
    /// doesn't fail the issuance.
    #[arg(long, default_value = "/var/lib/nixfleet-cp/issuance.log",
          env = "NIXFLEET_CP_AUDIT_LOG")]
    audit_log: PathBuf,

    /// SQLite database path. When set, the CP opens the DB at
    /// startup, runs migrations, and uses it for token replay + cert
    /// revocation + pending confirms + rollouts. When unset, in-memory
    /// state only — fine for dev/test, not for production.
    #[arg(long, env = "NIXFLEET_CP_DB_PATH")]
    db_path: Option<PathBuf>,

    /// Closure proxy upstream. Attic instance the CP forwards
    /// `/v1/agent/closure/<hash>` requests to. Typical value on lab:
    /// `http://localhost:8085` (attic on the same host). When unset,
    /// the closure proxy endpoint returns 501.
    #[arg(long, env = "NIXFLEET_CP_CLOSURE_UPSTREAM")]
    closure_upstream: Option<String>,
}

#[derive(Parser, Debug, Clone)]
struct TickFlags {
    #[arg(long)]
    artifact: PathBuf,

    #[arg(long)]
    signature: PathBuf,

    #[arg(long, default_value = "/etc/nixfleet/cp/trust.json")]
    trust_file: PathBuf,

    #[arg(long)]
    observed: PathBuf,

    #[arg(long, default_value_t = 86400)]
    freshness_window_secs: u64,
}

fn install_crypto_provider() {
    // Rustls 0.23 requires an explicit process-level CryptoProvider
    // when more than one crypto backend is compiled into the binary.
    // Our direct `rustls = "0.23"` dependency pulls in `aws-lc-rs`
    // (its default feature) while `reqwest` (dev-dep) with
    // `rustls-tls` pulls in `ring`. Without this call, the first
    // `ServerConfig::builder()` in `tls::build_server_config` panics
    // with "Could not automatically determine the process-level
    // CryptoProvider from Rustls crate features".
    //
    // `install_default` returns `Err` if a provider is already set
    // (e.g. test harness already installed one). Idempotent for our
    // purposes — the important thing is that *some* aws_lc_rs
    // provider is registered before we build a `ServerConfig`.
    //
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    install_crypto_provider();

    match Args::parse().command {
        Command::Serve(flags) => match run_serve(flags).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("serve: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::Tick(flags) => run_tick(flags),
    }
}

async fn run_serve(flags: ServeFlags) -> anyhow::Result<()> {
    let listen = flags
        .listen
        .parse()
        .map_err(|e| anyhow::anyhow!("--listen {}: {e}", flags.listen))?;

    // Channel-refs poll config: artifact + signature URLs gate
    // whether the poll is enabled. Partial config is rejected with a
    // clear error rather than silently falling back to file mode.
    // The token file is independently optional (public sources don't
    // need auth).
    let channel_refs = match (
        flags.channel_refs_artifact_url,
        flags.channel_refs_signature_url,
    ) {
        (Some(artifact_url), Some(signature_url)) => {
            Some(nixfleet_control_plane::channel_refs_poll::ChannelRefsSource {
                artifact_url,
                signature_url,
                token_file: flags.channel_refs_token_file,
                // Same trust + freshness as the file-backed reconcile
                // path. Read fresh on every poll so trust-root rotation
                // propagates without a CP restart.
                trust_path: flags.trust_file.clone(),
                freshness_window: Duration::from_secs(flags.freshness_window_secs),
            })
        }
        (None, None) => None,
        _ => {
            anyhow::bail!(
                "channel-refs poll: --channel-refs-artifact-url and \
                 --channel-refs-signature-url must be passed together (or both omitted)."
            );
        }
    };

    server::serve(server::ServeArgs {
        listen,
        tls_cert: flags.tls_cert,
        tls_key: flags.tls_key,
        client_ca: flags.client_ca,
        fleet_ca_cert: flags.fleet_ca_cert,
        fleet_ca_key: flags.fleet_ca_key,
        audit_log_path: Some(flags.audit_log),
        artifact_path: flags.artifact,
        signature_path: flags.signature,
        trust_path: flags.trust_file,
        observed_path: flags.observed,
        freshness_window: Duration::from_secs(flags.freshness_window_secs),
        channel_refs,
        db_path: flags.db_path,
        closure_upstream: flags.closure_upstream,
    })
    .await
}

fn run_tick(flags: TickFlags) -> ExitCode {
    let inputs = TickInputs {
        artifact_path: flags.artifact,
        signature_path: flags.signature,
        trust_path: flags.trust_file,
        observed_path: flags.observed,
        now: Utc::now(),
        freshness_window: Duration::from_secs(flags.freshness_window_secs),
    };

    let result = match tick(&inputs) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("tick: {err:#}");
            return ExitCode::from(2);
        }
    };

    print!("{}", render_plan(&result));

    match &result.verify {
        VerifyOutcome::Ok { actions, .. } => {
            tracing::info!(actions = actions.len(), "tick ok");
            ExitCode::SUCCESS
        }
        VerifyOutcome::Failed { reason } => {
            tracing::warn!(%reason, "verify failed");
            ExitCode::from(1)
        }
    }
}
