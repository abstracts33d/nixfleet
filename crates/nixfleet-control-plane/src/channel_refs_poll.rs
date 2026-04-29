//! Channel-refs poll loop.
//!
//! Polls a configured pair of URLs (artifact + signature) every 60s
//! for the signed `fleet.resolved.json`, runs the existing
//! `verify_artifact` against it, and refreshes the in-memory
//! verified-fleet snapshot + a `channel_refs` cache.
//!
//! Source-agnostic by design: the framework only knows how to issue
//! `GET <url>` with a Bearer token and parse the body as raw bytes.
//! Concrete URL shapes (Forgejo `/raw/branch/...`, GitHub
//! `raw.githubusercontent.com/...`, GitLab `/-/raw/...`, plain
//! HTTPS, etc.) are constructed by the consumer (or by a helper
//! exposed at `flake.scopes.gitops.<forge>` from this repo).
//!
//! Failure semantics: log warning + retain previous cache. CP does
//! not crash on source unavailability — operator can curl /healthz
//! and see when the last successful tick was even if the upstream
//! is down.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

use crate::signed_fetch;

/// Poll cadence — D9 default. Faster doesn't help (CI sign + push
/// latency dominates); slower delays the operator's "I pushed a
/// release commit, when does CP see it" feedback loop unhelpfully.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Configuration for the poll task. Source-agnostic — the consumer
/// supplies fully-formed URLs that yield raw artifact / signature
/// bytes when GET'd with the configured Bearer token.
#[derive(Debug, Clone)]
pub struct ChannelRefsSource {
    /// URL that yields the raw bytes of the canonical resolved
    /// artifact JSON. e.g.
    /// `https://git.example.com/myorg/myfleet/raw/branch/main/releases/fleet.resolved.json`
    /// (Forgejo) or
    /// `https://raw.githubusercontent.com/myorg/myfleet/main/releases/fleet.resolved.json`
    /// (GitHub).
    pub artifact_url: String,
    /// URL that yields the raw bytes of the matching signature.
    pub signature_url: String,
    /// Path to a file containing the API token (no surrounding
    /// whitespace). Sent as `Authorization: Bearer <token>`. Read on
    /// each poll so token rotation propagates without restart.
    /// Optional: leave None for unauthenticated public sources.
    pub token_file: Option<PathBuf>,
    /// Trust roots — read fresh on each poll so rotation in
    /// `trust.json` propagates without a CP restart.
    pub trust_path: PathBuf,
    /// Freshness window passed to `verify_artifact`. Same value the
    /// reconcile loop's file-backed verify path uses.
    pub freshness_window: Duration,
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
/// 1. Fetches artifact + signature from the configured URLs (over
///    HTTPS with the configured Bearer token, if any).
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
/// Failure semantics: log warn, retain previous state. A transient
/// outage or a bad signature must not blank out a previously-good
/// snapshot — the operator fixes the artifact, the next poll
/// repopulates.
pub fn spawn(
    cache: Arc<RwLock<ChannelRefsCache>>,
    verified_fleet: Arc<RwLock<Option<Arc<nixfleet_proto::FleetResolved>>>>,
    config: ChannelRefsSource,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = signed_fetch::build_client();

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

                    // Issue #49 — heartbeat at INFO on every successful
                    // tick (`changed` and `unchanged` both visible).
                    // Operators tailing `journalctl -u
                    // nixfleet-control-plane` need a positive signal
                    // that the poll is alive without cross-checking
                    // Forgejo access logs.
                    tracing::info!(
                        count = refs.len(),
                        changed = changed,
                        signed_at = ?new_signed_at,
                        ci_commit = ?new_ci_commit,
                        "channel-refs poll: verified-fleet snapshot refreshed",
                    );
                }
                Err(err) => {
                    // Cache + snapshot retained — reconcile loop
                    // continues against last-known good state.
                    tracing::warn!(
                        error = %err,
                        "channel-refs poll failed; retaining previous verified-fleet snapshot",
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
/// release than what's on the upstream (CI commits the [skip ci]
/// release AFTER building the closure, so each closure's bundled
/// artifact is the previous release). Agents check in immediately
/// on CP boot, before the periodic poll's first tick, and dispatch
/// returns a stale target — lab observed stair-stepping backwards
/// through deploy history during the GitOps validation pass.
///
/// Behaviour: this function tries the upstream path. On success the
/// caller stores the verified `FleetResolved` in `verified_fleet`.
/// On failure (network, verify, anything) the caller falls back to
/// the build-time artifact prime — same posture as before, just
/// with the upstream as the preferred source when configured.
pub async fn prime_once(
    config: &ChannelRefsSource,
) -> Result<nixfleet_proto::FleetResolved> {
    let client = signed_fetch::build_client();
    let (_refs, fleet) = poll_once(&client, config).await?;
    Ok(fleet)
}

async fn poll_once(
    client: &reqwest::Client,
    config: &ChannelRefsSource,
) -> Result<(HashMap<String, String>, nixfleet_proto::FleetResolved)> {
    let token = signed_fetch::read_token(config.token_file.as_deref())?;
    let (artifact_bytes, signature_bytes) = signed_fetch::fetch_signed_pair(
        client,
        &config.artifact_url,
        &config.signature_url,
        token.as_deref(),
    )
    .await?;

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
    .map_err(|e| anyhow::anyhow!("verify_artifact (channel-refs poll): {e:?}"))?;

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
