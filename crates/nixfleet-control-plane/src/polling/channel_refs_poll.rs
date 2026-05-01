//! Channel-refs poll: every 60s GET artifact + signature URLs,
//! verify, refresh the in-memory snapshot. Source-agnostic — only
//! knows `GET` + Bearer token. Failure retains previous state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::polling::signed_fetch;

/// CI sign+push latency dominates; faster polling doesn't help.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ChannelRefsSource {
    pub artifact_url: String,
    pub signature_url: String,
    /// Read on each poll so token rotation propagates without restart.
    /// None for unauthenticated public sources.
    pub token_file: Option<PathBuf>,
    /// Read fresh per poll so trust.json rotation propagates.
    pub trust_path: PathBuf,
    pub freshness_window: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelRefsCache {
    pub refs: HashMap<String, String>,
    pub last_refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Spawn the poll task. Failure logs warn + retains previous state
/// (transient outage / bad sig must not blank out a good snapshot).
pub fn spawn(
    cache: Arc<RwLock<ChannelRefsCache>>,
    verified_fleet: Arc<RwLock<Option<Arc<nixfleet_proto::FleetResolved>>>>,
    fleet_resolved_hash: Arc<RwLock<Option<String>>>,
    config: ChannelRefsSource,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = signed_fetch::build_client();

        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match poll_once(&client, &config).await {
                Ok((refs, fleet, fleet_hash)) => {
                    let new_signed_at = fleet.meta.signed_at;
                    let new_ci_commit = fleet.meta.ci_commit.clone();

                    {
                        let mut guard = verified_fleet.write().await;
                        *guard = Some(Arc::new(fleet));
                    }
                    {
                        let mut guard = fleet_resolved_hash.write().await;
                        *guard = Some(fleet_hash);
                    }

                    let mut guard = cache.write().await;
                    let changed = guard.refs != refs;
                    guard.refs = refs.clone();
                    guard.last_refreshed_at = Some(chrono::Utc::now());
                    drop(guard);

                    // INFO heartbeat per tick — operators need a
                    // positive signal the poll is alive.
                    tracing::info!(
                        count = refs.len(),
                        changed = changed,
                        signed_at = ?new_signed_at,
                        ci_commit = ?new_ci_commit,
                        "channel-refs poll: verified-fleet snapshot refreshed",
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "channel-refs poll failed; retaining previous verified-fleet snapshot",
                    );
                }
            }
        }
    })
}

/// One-shot fetch + verify before the reconcile loop starts. Without
/// this, dispatch on CP boot uses the compile-time `--artifact`,
/// which is always older than upstream (CI commits the release after
/// building the closure) — observed as stair-stepping backwards
/// through deploy history. Failure falls back to the build-time
/// artifact.
///
/// Returns `(fleet, fleet_resolved_hash)` — the hash anchors every
/// rolloutId derivation downstream (RFC-0002 §4.4).
pub async fn prime_once(
    config: &ChannelRefsSource,
) -> Result<(nixfleet_proto::FleetResolved, String)> {
    let client = signed_fetch::build_client();
    let (_refs, fleet, hash) = poll_once(&client, config).await?;
    Ok((fleet, hash))
}

async fn poll_once(
    client: &reqwest::Client,
    config: &ChannelRefsSource,
) -> Result<(
    HashMap<String, String>,
    nixfleet_proto::FleetResolved,
    String,
)> {
    let token = signed_fetch::read_token(config.token_file.as_deref())?;
    let (artifact_bytes, signature_bytes) = signed_fetch::fetch_signed_pair(
        client,
        &config.artifact_url,
        &config.signature_url,
        token.as_deref(),
    )
    .await?;

    let (trusted_keys, reject_before) = signed_fetch::read_trust_roots(&config.trust_path)?;

    let fleet_resolved = nixfleet_reconciler::verify_artifact(
        &artifact_bytes,
        &signature_bytes,
        &trusted_keys,
        chrono::Utc::now(),
        config.freshness_window,
        reject_before,
    )
    .map_err(|e| anyhow::anyhow!("verify_artifact (channel-refs poll): {e:?}"))?;

    // Compute the canonical-bytes hash that anchors every rolloutId
    // derivation downstream. Re-canonicalising the parsed FleetResolved
    // is byte-stable (same JCS implementation produced the bytes we
    // just verified), so this matches what producers and auditors get.
    let fleet_resolved_hash = nixfleet_reconciler::compute_canonical_hash(&fleet_resolved)
        .map_err(|e| anyhow::anyhow!("compute_canonical_hash: {e:?}"))?;

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
    Ok((refs, fleet_resolved, fleet_resolved_hash))
}
