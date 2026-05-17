//! Periodic agent-side manifest poll. Mirrors CP's
//! `runtime::workers::manifest_poll`: every 30s the worker fetches
//! `/v1/fleet.resolved` + each channel's `/v1/rollouts/{id}` from CP,
//! verifies signatures + cross-checks each rollout's
//! `fleet_resolved_hash` against the canonical hash of the just-fetched
//! fleet, then emits `ReducerInput::ManifestSetUpdated` so the reducer's
//! cached `SignedManifestSet` reflects the current fleet snapshot.
//!
//! First tick fires at `Instant::now()` (no startup delay), so the
//! reducer's `manifests` cache is populated before the longpoll worker
//! receives its first `Dispatch`. Subsequent ticks every 30s; on tick
//! failure the worker retains the prior emit and retries next tick.
//!
//! Failure semantics (matching CP's manifest_poll):
//! - Fleet fetch/verify failed -> skip the entire tick; reducer retains
//!   its prior cache.
//! - Per-rollout fetch/verify failed OR cross-check mismatch -> log +
//!   skip that channel only; emit the partial set.
//! - Reducer channel closed (cancel propagated) -> exit.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use nixfleet_reconciler::compute_rollout_id_for_channel;
use nixfleet_reconciler::planner_types::SignedManifestSet;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::{AgentConfig, ReducerInput, ShutdownToken};
use crate::manifest_cache::{DEFAULT_TRUST_PATH, ManifestCache};

/// Poll cadence. Matches CP's `runtime::workers::manifest_poll::POLL_INTERVAL`.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub fn spawn(
    cfg: AgentConfig,
    clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    spawn_with_trust_path(
        cfg,
        Path::new(DEFAULT_TRUST_PATH).to_path_buf(),
        clock,
        input_tx,
        shutdown,
    )
}

/// Tunable-trust-path entry point. Production code calls [`spawn`] which
/// pins [`DEFAULT_TRUST_PATH`] (RFC-0005 §1.5 convention); integration
/// tests under the `test-helpers` feature gate call this variant with a
/// tempdir-rooted trust.json so the worker can run without touching
/// `/etc/nixfleet/agent/`. Same convention as
/// `nixfleet_agent::runtime::ShutdownToken::__test_only_from_rx`.
pub fn spawn_with_trust_path(
    cfg: AgentConfig,
    trust_path: std::path::PathBuf,
    _clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        let client = match crate::comms::build_client(
            cfg.ca_cert.as_deref(),
            cfg.client_cert.as_deref(),
            cfg.client_key.as_deref(),
        ) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(
                    target: "agent_manifest_poll",
                    error = %err,
                    "failed to build mTLS HTTP client; worker exits",
                );
                return;
            }
        };
        let manifest_cache = ManifestCache::new(&cfg.state_dir, &trust_path);
        let cp_url = cfg.control_plane_url.clone();

        // First tick at Instant::now() (no startup delay) so the reducer's
        // manifest cache is populated before longpoll's first dispatch can
        // arrive. Subsequent ticks every POLL_INTERVAL.
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_manifest_poll",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {}
            }

            match tick_once(&manifest_cache, &client, &cp_url).await {
                Ok(set) => {
                    if let Err(err) = input_tx
                        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
                        .await
                    {
                        tracing::warn!(
                            target: "agent_manifest_poll",
                            error = %err,
                            "reducer input channel closed; worker exits",
                        );
                        return;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        target: "agent_manifest_poll",
                        error = %err,
                        "tick failed; retaining previous reducer cache",
                    );
                }
            }
        }
    })
}

async fn tick_once(
    manifest_cache: &ManifestCache,
    client: &reqwest::Client,
    cp_url: &str,
) -> anyhow::Result<SignedManifestSet> {
    // 1. Fetch + verify fleet.resolved. `fetch_or_load_fleet` returns the
    //    canonical hash of the artifact bytes paired with the Verified
    //    struct so the per-rollout discriminator below has its anchor.
    let (verified_fleet, fleet_hash) = manifest_cache
        .fetch_or_load_fleet(client, cp_url)
        .await
        .map_err(|err| anyhow::anyhow!("fetch fleet: {}", err.reason()))?;

    // 2. For each channel in the verified fleet, derive the expected
    //    rollout_id, fetch + verify, and cross-check the discriminator.
    let mut rollouts = HashMap::new();
    for channel in verified_fleet.inner().channels.keys() {
        let rollout_id =
            match compute_rollout_id_for_channel(verified_fleet.inner(), &fleet_hash, channel) {
                Ok(Some(id)) => id,
                Ok(None) => continue, // channel has no host with a closure yet
                Err(err) => {
                    tracing::warn!(
                        target: "agent_manifest_poll",
                        %channel,
                        error = %err,
                        "compute_rollout_id_for_channel failed; skipping channel",
                    );
                    continue;
                }
            };

        let verified_rollout = match manifest_cache
            .fetch_or_load(client, cp_url, &rollout_id)
            .await
        {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "agent_manifest_poll",
                    %channel,
                    %rollout_id,
                    error = %err.reason(),
                    "fetch rollout manifest failed; skipping channel",
                );
                continue;
            }
        };

        // Discriminator: the rollout's pinned fleet_resolved_hash MUST
        // match the canonical hash of the fleet we just fetched. A
        // mismatch means CP is serving inconsistent state across the
        // two artifacts (e.g. fresh fleet snapshot but stale rollout
        // pin, or vice versa). Defense-in-depth over signature verify;
        // mirrors the architect's amendment on the d010-feed plan
        // restated under Option C: discriminator lives at the worker
        // boundary as a cross-consistency check.
        if verified_rollout.inner().fleet_resolved_hash != fleet_hash {
            tracing::warn!(
                target: "agent_manifest_poll",
                %channel,
                %rollout_id,
                rollout_pin = %verified_rollout.inner().fleet_resolved_hash,
                fleet_hash = %fleet_hash,
                "rollout manifest pins a different fleet snapshot than the one CP served; skipping",
            );
            continue;
        }

        rollouts.insert(channel.clone(), verified_rollout);
    }

    Ok(SignedManifestSet::new(verified_fleet, rollouts))
}
