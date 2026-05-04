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
    db: Option<Arc<crate::db::Db>>,
    config: ChannelRefsSource,
) -> tokio::task::JoinHandle<()> {
    SignedArtifactPoller {
        interval: POLL_INTERVAL,
        label: "channel-refs",
    }
    .spawn(cancel, move |client| {
        let cache = Arc::clone(&cache);
        let verified_fleet = Arc::clone(&verified_fleet);
        let db = db.clone();
        let config = config.clone();
        async move {
            let (refs, fleet, fleet_hash) = poll_once(&client, &config).await?;
            apply_verified_refs(&cache, &verified_fleet, db.as_deref(), refs, fleet, fleet_hash).await;
            Ok(())
        }
    })
}

/// (fleet, fleet_resolved_hash) under one RwLock so readers never see a torn
/// snapshot. After committing the snapshot, record each channel's current
/// rollout_id in the rollouts table (LOADBEARING for rebuild recovery —
/// this is the path that repopulates rollouts soft state without needing
/// any host to check in first).
async fn apply_verified_refs(
    cache: &RwLock<ChannelRefsCache>,
    verified_fleet: &RwLock<Option<crate::server::VerifiedFleetSnapshot>>,
    db: Option<&crate::db::Db>,
    refs: HashMap<String, String>,
    fleet: nixfleet_proto::FleetResolved,
    fleet_hash: String,
) {
    let new_signed_at = fleet.meta.signed_at;
    let new_ci_commit = fleet.meta.ci_commit.clone();

    // Compute current rollout_id per channel against the same snapshot bytes
    // we're about to publish, BEFORE moving fleet into the Arc. Channels
    // with no host having a closure declaration return Ok(None) — skip.
    let channel_rollouts: Vec<(String, String)> = if db.is_some() {
        fleet
            .channels
            .keys()
            .filter_map(|channel| {
                match nixfleet_reconciler::compute_rollout_id_for_channel(
                    &fleet,
                    &fleet_hash,
                    channel,
                ) {
                    Ok(Some(rid)) => Some((channel.clone(), rid)),
                    Ok(None) => None,
                    Err(err) => {
                        tracing::warn!(
                            channel = %channel,
                            error = %err,
                            "channel-refs poll: compute_rollout_id_for_channel failed; skipping",
                        );
                        None
                    }
                }
            })
            .collect()
    } else {
        Vec::new()
    };

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

    if let Some(db) = db {
        record_rollouts_gated_by_channel_edges(db, verified_fleet, &channel_rollouts).await;
    }

    tracing::info!(
        count = refs.len(),
        changed = changed,
        signed_at = ?new_signed_at,
        ci_commit = ?new_ci_commit,
        active_rollouts_recorded = channel_rollouts.len(),
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

    // Anchors every downstream rolloutId derivation. Hash the received
    // artifact bytes (NOT a re-serialised parsed struct) so additive
    // schema changes the CP's proto doesn't yet know about don't shift
    // the anchor relative to what CI signed. Same load-bearing reason
    // as the rollouts route's verify_content_address.
    let fleet_resolved_hash = nixfleet_reconciler::canonical_hash_from_bytes(&artifact_bytes)
        .map_err(|e| anyhow::anyhow!("canonical_hash_from_bytes (fleet.resolved): {e:?}"))?;

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

/// Gate `record_active_rollout` writes by `channelEdges`. The DB rollouts
/// table is the source of truth for /v1/rollouts and the reconciler's
/// `Observed.active_rollouts`; recording an entry for a channel whose
/// predecessor hasn't converged would defeat the channelEdges contract
/// at the storage layer (the reconciler's `RolloutDeferred` is journal-
/// only and doesn't touch the DB).
///
/// Iteration order is the same topological sort the reconciler uses, with
/// an in-poll `emitted_opens` set so a `before` channel recorded earlier
/// in this poll is seen as active by `after`'s predecessor check. This
/// mirrors the reconcile loop's invariants — the two layers stay
/// architecturally aligned.
async fn record_rollouts_gated_by_channel_edges(
    db: &crate::db::Db,
    verified_fleet: &RwLock<Option<crate::server::VerifiedFleetSnapshot>>,
    channel_rollouts: &[(String, String)],
) {
    let fleet_snap = match verified_fleet.read().await.clone() {
        Some(s) => s,
        None => return,
    };
    let fleet = &fleet_snap.fleet;

    // Build the same `Observed.active_rollouts` view the reconciler sees,
    // so `predecessor_channel_blocking` resolves identically.
    let raw = match db.host_dispatch_state().active_rollouts_snapshot() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "channel-refs poll: active_rollouts_snapshot failed; recording rollouts without channelEdges gate (non-fatal)");
            Vec::new()
        }
    };
    let superseded: std::collections::HashSet<String> = db
        .rollouts()
        .superseded_rollout_ids()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let active_rollouts: Vec<nixfleet_reconciler::observed::Rollout> = raw
        .into_iter()
        .filter(|r| !superseded.contains(&r.rollout_id))
        .map(|snap| nixfleet_reconciler::observed::Rollout {
            id: snap.rollout_id,
            channel: snap.channel,
            target_ref: snap.target_channel_ref,
            state: nixfleet_reconciler::RolloutState::Executing,
            current_wave: snap.current_wave as usize,
            host_states: snap
                .host_states
                .iter()
                .filter_map(|(h, s)| {
                    nixfleet_reconciler::HostRolloutState::from_db_str(s)
                        .ok()
                        .map(|st| (h.clone(), st))
                })
                .collect(),
            last_healthy_since: snap.last_healthy_since,
            budgets: vec![],
        })
        .collect();

    let pseudo_observed = nixfleet_reconciler::observed::Observed {
        channel_refs: HashMap::new(),
        last_rolled_refs: HashMap::new(),
        host_state: HashMap::new(),
        active_rollouts,
        compliance_failures_by_rollout: HashMap::new(),
        last_deferrals: HashMap::new(),
    };

    let channel_names: Vec<String> = channel_rollouts.iter().map(|(c, _)| c.clone()).collect();
    let order = nixfleet_reconciler::topological_channel_order(fleet, &channel_names);

    let mut emitted_opens: std::collections::HashSet<String> = std::collections::HashSet::new();
    for channel in order {
        let Some((_, rid)) = channel_rollouts.iter().find(|(c, _)| c == &channel) else {
            continue;
        };
        if let Some(blocker) = nixfleet_reconciler::predecessor_channel_blocking(
            fleet,
            &pseudo_observed,
            &emitted_opens,
            &channel,
        ) {
            tracing::info!(
                channel = %channel,
                rollout = %rid,
                blocked_by = %blocker,
                "channel-refs poll: skip record_active_rollout — channelEdges holds until predecessor converges",
            );
            continue;
        }
        if let Err(err) = db.rollouts().record_active_rollout(rid, &channel) {
            tracing::warn!(
                channel = %channel,
                rollout = %rid,
                error = %err,
                "channel-refs poll: record_active_rollout failed (non-fatal)",
            );
        } else {
            emitted_opens.insert(channel);
        }
    }
}
