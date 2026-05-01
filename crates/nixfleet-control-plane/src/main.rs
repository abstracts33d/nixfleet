#![allow(clippy::doc_lazy_continuation)]
//! `nixfleet-control-plane` CLI: `serve` (long-running TLS server +
//! 30s reconcile loop) or `tick` (oneshot verify + reconcile + plan).
//!
//! `tick` exit codes: 0 = ok, 1 = verify failed, 2 = pre-verify error.

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
    /// Boxed to keep Command cheap-to-move; ServeFlags is ~470 bytes.
    Serve(Box<ServeFlags>),
    Tick(TickFlags),
}

#[derive(Parser, Debug, Clone)]
struct ServeFlags {
    #[arg(long, default_value = "0.0.0.0:8080", env = "NIXFLEET_CP_LISTEN")]
    listen: String,

    #[arg(long, env = "NIXFLEET_CP_TLS_CERT")]
    tls_cert: PathBuf,

    #[arg(long, env = "NIXFLEET_CP_TLS_KEY")]
    tls_key: PathBuf,

    /// When set, server requires verified mTLS.
    #[arg(long, env = "NIXFLEET_CP_CLIENT_CA")]
    client_ca: Option<PathBuf>,

    #[arg(long)]
    artifact: PathBuf,

    #[arg(long)]
    signature: PathBuf,

    #[arg(long, default_value = "/etc/nixfleet/cp/trust.json")]
    trust_file: PathBuf,

    /// Dev/test fallback; in-memory projection from check-ins is
    /// preferred at runtime.
    #[arg(long)]
    observed: PathBuf,

    #[arg(long, default_value_t = 86400)]
    freshness_window_secs: u64,

    /// Default 360s: ~300s agent poll budget + 60s slack. Dropping
    /// below ~310s creates chaos (CP rolls back while agent polls).
    #[arg(long, default_value_t = 360)]
    confirm_deadline_secs: i64,

    // Channel-refs poll. The artifact + signature URLs together gate
    // whether the poll task is spawned — set both to enable, leave
    // both unset to fall back to file-backed channel-refs from
    // observed.json. Source-agnostic: any HTTP(S) URL that yields the
    // raw bytes of the artifact / signature works (Forgejo `raw`
    // path, GitHub `raw.githubusercontent.com`, GitLab `/-/raw/...`,
    // a plain file server, etc.). Concrete URL templates for common
    // forges live in this repo's `impls/gitops/` and are exposed at
    // `flake.scopes.gitops.<forge>`.
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

    /// When unset, revocations polling is disabled.
    #[arg(long, env = "NIXFLEET_CP_REVOCATIONS_ARTIFACT_URL")]
    revocations_artifact_url: Option<String>,

    #[arg(long, env = "NIXFLEET_CP_REVOCATIONS_SIGNATURE_URL")]
    revocations_signature_url: Option<String>,

    /// Defaults to the channel-refs token when unset.
    #[arg(long, env = "NIXFLEET_CP_REVOCATIONS_TOKEN_FILE")]
    revocations_token_file: Option<PathBuf>,

    /// When unset, /v1/enroll and /v1/agent/renew return 500.
    #[arg(long, env = "NIXFLEET_CP_FLEET_CA_CERT")]
    fleet_ca_cert: Option<PathBuf>,

    #[arg(long, env = "NIXFLEET_CP_FLEET_CA_KEY")]
    fleet_ca_key: Option<PathBuf>,

    /// JSON-lines, one record per issuance. Best-effort writes.
    #[arg(
        long,
        default_value = "/var/lib/nixfleet-cp/issuance.log",
        env = "NIXFLEET_CP_AUDIT_LOG"
    )]
    audit_log: PathBuf,

    /// When unset, in-memory state only.
    #[arg(long, env = "NIXFLEET_CP_DB_PATH")]
    db_path: Option<PathBuf>,

    /// Base URL of any nix-cache-protocol server (harmonia, attic,
    /// cachix, nix-serve, …). When unset, returns 501.
    #[arg(long, env = "NIXFLEET_CP_CLOSURE_UPSTREAM")]
    closure_upstream: Option<String>,

    /// Filesystem directory containing pre-signed rollout manifests
    /// (`<rolloutId>.{json,sig}`) produced by `nixfleet-release`.
    /// `GET /v1/rollouts/<rolloutId>` looks up files here first. When
    /// the file is absent, falls back to `--rollouts-source-*`
    /// templates if configured. When BOTH are unset, manifest
    /// distribution is disabled (503).
    #[arg(long, env = "NIXFLEET_CP_ROLLOUTS_DIR")]
    rollouts_dir: Option<PathBuf>,

    /// URL template for HTTP-fetched rollout manifests. Must contain
    /// the literal `{rolloutId}` token; the CP substitutes the
    /// requested rolloutId and GETs the resulting URL when the
    /// filesystem source misses. Source-agnostic (Forgejo `/raw/`,
    /// GitHub raw, GitLab `/-/raw/`, plain HTTP). Pairs with
    /// `--rollouts-source-signature-url-template`.
    #[arg(long, env = "NIXFLEET_CP_ROLLOUTS_SOURCE_ARTIFACT_URL_TEMPLATE")]
    rollouts_source_artifact_url_template: Option<String>,

    /// Signature URL template (same `{rolloutId}` substitution as
    /// the artifact template). Both required to enable HTTP fetch.
    #[arg(long, env = "NIXFLEET_CP_ROLLOUTS_SOURCE_SIGNATURE_URL_TEMPLATE")]
    rollouts_source_signature_url_template: Option<String>,

    /// Bearer token file for the rollouts source. Defaults to the
    /// channel-refs token when unset (typical case: same Forgejo
    /// instance, same access token).
    #[arg(long, env = "NIXFLEET_CP_ROLLOUTS_SOURCE_TOKEN_FILE")]
    rollouts_source_token_file: Option<PathBuf>,

    /// Refuse to start when any security-relevant flag is unset:
    /// `--client-ca` (no mTLS verification), revocations source
    /// (silently re-validates revoked certs after a CP rebuild), or
    /// the protocol-version header on incoming requests. Default
    /// `false` for v0.2 to preserve current behaviour; intended for
    /// production. See #70 / `docs/CONTRACTS.md §II`.
    #[arg(long, env = "NIXFLEET_CP_STRICT")]
    strict: bool,
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
    // Rustls 0.23 requires an explicit provider when multiple
    // backends are linked (rustls→aws-lc-rs + reqwest→ring here).
    // `install_default` is idempotent against existing providers.
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
        Command::Serve(flags) => match run_serve(*flags).await {
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
            Some(
                nixfleet_control_plane::polling::channel_refs_poll::ChannelRefsSource {
                    artifact_url,
                    signature_url,
                    token_file: flags.channel_refs_token_file.clone(),
                    // Same trust + freshness as the file-backed reconcile
                    // path. Read fresh on every poll so trust-root rotation
                    // propagates without a CP restart.
                    trust_path: flags.trust_file.clone(),
                    freshness_window: Duration::from_secs(flags.freshness_window_secs),
                },
            )
        }
        (None, None) => None,
        _ => {
            anyhow::bail!(
                "channel-refs poll: --channel-refs-artifact-url and \
                 --channel-refs-signature-url must be passed together (or both omitted)."
            );
        }
    };

    // Rollouts HTTP source: optional companion to the filesystem
    // path. Both URL templates must come together (or both omitted);
    // token file falls back to the channel-refs one — the typical
    // case is one Forgejo instance with one access token serving all
    // three sidecars (fleet.resolved, revocations, rollouts).
    let rollouts_source = match (
        flags.rollouts_source_artifact_url_template.clone(),
        flags.rollouts_source_signature_url_template.clone(),
    ) {
        (Some(artifact_tpl), Some(signature_tpl)) => Some(
            nixfleet_control_plane::rollouts_source::RolloutsSource::new(
                artifact_tpl,
                signature_tpl,
                flags
                    .rollouts_source_token_file
                    .clone()
                    .or(flags.channel_refs_token_file.clone()),
            )?,
        ),
        (None, None) => None,
        _ => {
            anyhow::bail!(
                "rollouts source: --rollouts-source-artifact-url-template and \
                 --rollouts-source-signature-url-template must be passed together \
                 (or both omitted)."
            );
        }
    };

    // Revocations poll config: same artifact/signature pair gating
    // pattern. Token file falls back to the channel-refs one when
    // unspecified; trust + freshness are shared.
    let revocations = match (
        flags.revocations_artifact_url,
        flags.revocations_signature_url,
    ) {
        (Some(artifact_url), Some(signature_url)) => Some(
            nixfleet_control_plane::polling::revocations_poll::RevocationsSource {
                artifact_url,
                signature_url,
                token_file: flags
                    .revocations_token_file
                    .or(flags.channel_refs_token_file.clone()),
                trust_path: flags.trust_file.clone(),
                freshness_window: Duration::from_secs(flags.freshness_window_secs),
            },
        ),
        (None, None) => None,
        _ => {
            anyhow::bail!(
                "revocations poll: --revocations-artifact-url and \
                 --revocations-signature-url must be passed together (or both omitted)."
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
        confirm_deadline_secs: flags.confirm_deadline_secs,
        channel_refs,
        revocations,
        rollouts_source,
        db_path: flags.db_path,
        closure_upstream: flags.closure_upstream,
        rollouts_dir: flags.rollouts_dir,
        strict: flags.strict,
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
        VerifyOutcome::Ok(ok) => {
            tracing::info!(actions = ok.actions.len(), "tick ok");
            ExitCode::SUCCESS
        }
        VerifyOutcome::Failed { reason } => {
            tracing::warn!(%reason, "verify failed");
            ExitCode::from(1)
        }
    }
}
