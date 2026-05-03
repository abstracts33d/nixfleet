//! Channel-refs poll: every 60s fetch + verify; failure retains previous state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::polling::poller::SignedArtifactPoller;
use crate::polling::signed_fetch;

/// CI sign+push latency dominates; faster polling doesn't help.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct ChannelRefsSource {
    pub artifact_url: String,
    pub signature_url: String,
    /// Re-read each poll so token rotation propagates without restart.
    pub token_file: Option<PathBuf>,
    /// Re-read each poll so trust.json rotation propagates.
    pub trust_path: PathBuf,
    pub freshness_window: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelRefsCache {
    pub refs: HashMap<String, String>,
    pub last_refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Failure retains previous state; cancel exits the loop cleanly.
pub fn spawn(
    cancel: tokio_util::sync::CancellationToken,
    cache: Arc<RwLock<ChannelRefsCache>>,
    verified_fleet: Arc<RwLock<Option<crate::server::VerifiedFleetSnapshot>>>,
    config: ChannelRefsSource,
) -> tokio::task::JoinHandle<()> {
    SignedArtifactPoller {
        interval: POLL_INTERVAL,
        label: "channel-refs",
    }
    .spawn(cancel, move |client| {
        let cache = Arc::clone(&cache);
        let verified_fleet = Arc::clone(&verified_fleet);
        let config = config.clone();
        async move {
            let (refs, fleet, fleet_hash) = poll_once(&client, &config).await?;
            apply_verified_refs(&cache, &verified_fleet, refs, fleet, fleet_hash).await;
            Ok(())
        }
    })
}

/// (fleet, fleet_resolved_hash) under one RwLock so readers never see a torn snapshot.
async fn apply_verified_refs(
    cache: &RwLock<ChannelRefsCache>,
    verified_fleet: &RwLock<Option<crate::server::VerifiedFleetSnapshot>>,
    refs: HashMap<String, String>,
    fleet: nixfleet_proto::FleetResolved,
    fleet_hash: String,
) {
    let new_signed_at = fleet.meta.signed_at;
    let new_ci_commit = fleet.meta.ci_commit.clone();

    {
        let mut guard = verified_fleet.write().await;
        *guard = Some(crate::server::VerifiedFleetSnapshot {
            fleet: Arc::new(fleet),
            fleet_resolved_hash: fleet_hash,
        });
    }

    let mut guard = cache.write().await;
    let changed = guard.refs != refs;
    guard.refs = refs.clone();
    guard.last_refreshed_at = Some(chrono::Utc::now());
    drop(guard);

    tracing::info!(
        count = refs.len(),
        changed = changed,
        signed_at = ?new_signed_at,
        ci_commit = ?new_ci_commit,
        "channel-refs poll: verified-fleet snapshot refreshed",
    );
}

/// One-shot fetch+verify at boot; without it dispatch uses the stale build-time artifact.
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

    // Anchors every downstream rolloutId derivation; byte-stable JCS round-trip.
    let fleet_resolved_hash = nixfleet_reconciler::compute_canonical_hash(&fleet_resolved)
        .map_err(|e| anyhow::anyhow!("compute_canonical_hash: {e:?}"))?;

    // Single signing rev: every channel maps to the same CI commit.
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
