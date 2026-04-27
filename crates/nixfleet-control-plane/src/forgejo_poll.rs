//! Forgejo poll loop for channel-refs.
//!
//! Polls Forgejo's contents API every 60s for
//! `releases/fleet.resolved.json`, decodes the base64 body, runs the
//! existing `verify_artifact` against it, and refreshes an in-memory
//! `channel_refs` cache.
//!
//! Failure semantics: log warning + retain previous cache. CP does
//! not crash on Forgejo unavailability — operator can curl /healthz
//! and see when the last successful tick was even if Forgejo is
//! down.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;
use tokio::sync::RwLock;

/// Poll cadence — D9 default. Faster doesn't help (CI sign + push
/// latency dominates); slower delays the operator's "I pushed a
/// release commit, when does CP see it" feedback loop unhelpfully.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Configuration for the poll task. All fields populated by the
/// CLI flags in main.rs.
#[derive(Debug, Clone)]
pub struct ForgejoConfig {
    /// e.g. `https://git.lab.internal`. No trailing slash needed —
    /// the URL builder normalises.
    pub base_url: String,
    /// e.g. `abstracts33d`.
    pub owner: String,
    /// e.g. `fleet`.
    pub repo: String,
    /// Path inside the repo to the canonical resolved-artifact JSON.
    /// Default: `releases/fleet.resolved.json`.
    pub artifact_path: String,
    /// Path inside the repo to the matching signature.
    /// Default: `releases/fleet.resolved.json.sig`.
    pub signature_path: String,
    /// Path to a file containing the Forgejo API token (no surrounding
    /// whitespace). Read on each poll so token rotation propagates
    /// without restart. Loaded into memory at request time.
    pub token_file: PathBuf,
    /// Trust roots — read fresh on each poll so rotation in
    /// `trust.json` propagates without a CP restart.
    pub trust_path: PathBuf,
    /// Freshness window passed to `verify_artifact`. Same value the
    /// reconcile loop's file-backed verify path uses.
    pub freshness_window: Duration,
}

/// Forgejo `/api/v1/repos/{o}/{r}/contents/{path}` response.
/// `content` is base64-encoded with `\n` chunked every 60 chars
/// (RFC 2045 / "MIME" encoding).
#[derive(Debug, Deserialize)]
struct ContentsResponse {
    content: String,
    encoding: String,
}

/// In-memory cache the reconcile loop reads from. Wrapped in
/// `Arc<RwLock<...>>` so concurrent reads are cheap; writes only
/// happen at poll cadence.
#[derive(Debug, Clone, Default)]
pub struct ChannelRefsCache {
    pub refs: HashMap<String, String>,
    /// rfc3339 wall-clock of the last *successful* poll. None if
    /// we've never had one.
    pub last_refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Spawn the poll task. Runs forever; logs warnings on failure;
/// updates the channel-refs cache + the verified-fleet snapshot on
/// success.
///
/// On each successful poll the task:
/// 1. Fetches `releases/fleet.resolved.json` + its `.sig` from
///    Forgejo (over HTTPS with the deployed cp-forgejo-token).
/// 2. Reads `trust.json` fresh — operator key rotation propagates
///    on the next poll, no CP restart required.
/// 3. Runs `verify_artifact` (canonicalize + signature verify +
///    schemaVersion gate + freshness check). Same path the
///    reconcile loop's file-backed verifier uses.
/// 4. Updates `verified_fleet` so the dispatch path's per-checkin
///    decisions read fresh closureHashes.
/// 5. Refreshes the channel_refs cache (kept for telemetry +
///    `Observed.channel_refs` projection in the reconciler).
///
/// Failure semantics match the prior shape: log warn, retain
/// previous state. A transient Forgejo outage or a bad signature
/// must not blank out a previously-good snapshot — the operator
/// fixes the artifact, the next poll repopulates.
pub fn spawn(
    cache: Arc<RwLock<ChannelRefsCache>>,
    verified_fleet: Arc<RwLock<Option<Arc<nixfleet_proto::FleetResolved>>>>,
    config: ForgejoConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("build forgejo poll client");

        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match poll_once(&client, &config).await {
                Ok((refs, fleet)) => {
                    let new_signed_at = fleet.meta.signed_at;
                    let new_ci_commit = fleet.meta.ci_commit.clone();

                    // Snapshot first; channel_refs is a side concern
                    // the reconciler reads alongside the verified
                    // snapshot.
                    {
                        let mut guard = verified_fleet.write().await;
                        *guard = Some(Arc::new(fleet));
                    }

                    let mut guard = cache.write().await;
                    let changed = guard.refs != refs;
                    guard.refs = refs.clone();
                    guard.last_refreshed_at = Some(chrono::Utc::now());
                    drop(guard);

                    if changed {
                        tracing::info!(
                            count = refs.len(),
                            signed_at = ?new_signed_at,
                            ci_commit = ?new_ci_commit,
                            "forgejo poll: verified-fleet snapshot refreshed (channels changed)",
                        );
                    } else {
                        tracing::debug!(
                            count = refs.len(),
                            signed_at = ?new_signed_at,
                            ci_commit = ?new_ci_commit,
                            "forgejo poll: verified-fleet snapshot refreshed (channels unchanged)",
                        );
                    }
                }
                Err(err) => {
                    // Cache + snapshot retained — reconcile loop
                    // continues against last-known good state.
                    tracing::warn!(
                        error = %err,
                        "forgejo poll failed; retaining previous verified-fleet snapshot",
                    );
                }
            }
        }
    })
}

/// One-shot synchronous fetch + verify, called once from `serve()`
/// **before** starting the reconcile loop or accepting connections.
///
/// Without this, the CP's first reconcile-loop prime falls back to
/// the compile-time `--artifact` path — which is always an older
/// release than what's on Forgejo (CI commits the [skip ci] release
/// AFTER building the closure, so each closure's bundled artifact
/// is the previous release). Agents check in immediately on CP
/// boot, before the periodic poll's first tick, and dispatch
/// returns a stale target — lab observed stair-stepping backwards
/// through deploy history during the GitOps validation pass.
///
/// Behaviour: this function tries the Forgejo path. On success the
/// caller stores the verified `FleetResolved` in `verified_fleet`.
/// On failure (network, verify, anything) the caller falls back to
/// the build-time artifact prime — same posture as before, just
/// with Forgejo as the preferred source when configured.
pub async fn prime_once(
    config: &ForgejoConfig,
) -> Result<nixfleet_proto::FleetResolved> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build forgejo prime client")?;
    let (_refs, fleet) = poll_once(&client, config).await?;
    Ok(fleet)
}

async fn poll_once(
    client: &reqwest::Client,
    config: &ForgejoConfig,
) -> Result<(HashMap<String, String>, nixfleet_proto::FleetResolved)> {
    let token = std::fs::read_to_string(&config.token_file)
        .with_context(|| format!("read forgejo token file {}", config.token_file.display()))?
        .trim()
        .to_string();

    let artifact_bytes =
        fetch_repo_file(client, config, &token, &config.artifact_path).await?;
    let signature_bytes =
        fetch_repo_file(client, config, &token, &config.signature_path).await?;

    // Trust roots are read fresh on every poll. Operator rotates
    // `nixfleet.trust.ciReleaseKey.current` → next deploy
    // materialises the new trust.json → next poll picks it up. No
    // CP restart needed.
    let trust_raw = std::fs::read_to_string(&config.trust_path)
        .with_context(|| format!("read trust file {}", config.trust_path.display()))?;
    let trust: nixfleet_proto::TrustConfig =
        serde_json::from_str(&trust_raw).context("parse trust file")?;
    let trusted_keys = trust.ci_release_key.active_keys();
    let reject_before = trust.ci_release_key.reject_before;

    let fleet_resolved = nixfleet_reconciler::verify_artifact(
        &artifact_bytes,
        &signature_bytes,
        &trusted_keys,
        chrono::Utc::now(),
        config.freshness_window,
        reject_before,
    )
    .map_err(|e| anyhow::anyhow!("verify_artifact (forgejo poll): {e:?}"))?;

    // Flatten channels → channel_refs (telemetry + the shape the
    // reconciler's `Observed.channel_refs` expects). For now every
    // channel gets the same CI commit — single fleet repo, single
    // signing rev. Multi-channel-rev semantics (e.g. dev tracks main,
    // prod tracks a release branch) would split this map per channel.
    let ci_commit = fleet_resolved
        .meta
        .ci_commit
        .clone()
        .unwrap_or_else(|| "<unknown>".to_string());
    let mut refs = HashMap::new();
    for name in fleet_resolved.channels.keys() {
        refs.insert(name.clone(), ci_commit.clone());
    }
    Ok((refs, fleet_resolved))
}

/// Fetch a single file from a Forgejo repo via the Contents API.
/// Returns the raw decoded bytes (Forgejo serves base64 in its
/// `content` field; we strip the wrapping newlines + decode). One
/// helper shared between artifact + signature reads so the URL +
/// auth + decoding logic lives in one place.
async fn fetch_repo_file(
    client: &reqwest::Client,
    config: &ForgejoConfig,
    token: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let url = format!(
        "{}/api/v1/repos/{}/{}/contents/{}",
        config.base_url.trim_end_matches('/'),
        config.owner,
        config.repo,
        path,
    );

    let resp = client
        .get(&url)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{url}: {status}: {body}");
    }

    let parsed: ContentsResponse =
        resp.json().await.with_context(|| format!("parse forgejo contents {url}"))?;
    if parsed.encoding != "base64" {
        anyhow::bail!("{url}: unexpected forgejo content encoding {}", parsed.encoding);
    }

    base64::engine::general_purpose::STANDARD
        .decode(parsed.content.replace(['\n', '\r'], "").as_bytes())
        .with_context(|| format!("decode forgejo base64 content {url}"))
}
