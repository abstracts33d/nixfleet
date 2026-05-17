//! Manifest poll worker: verifies channel-refs (fleet.resolved) + per-channel
//! rollout manifests on a 30s tick. Two side effects per successful tick:
//!
//!   1. Emits [`super::super::ReducerInput::ManifestSetUpdated`] so the
//!      reducer task refreshes its cached `SignedManifestSet` and re-runs
//!      `plan_next`.
//!   2. Writes the verified [`crate::server::VerifiedFleetSnapshot`] into
//!      `state.verified_fleet`. The fleet manifest exists in two homes
//!      because the legacy operator-API routes (`channel_status`,
//!      `whoami`, the rollouts proxy) still read from `state.verified_fleet`
//!      synchronously; consolidating onto the runtime cache is a separate
//!      effort. Both writes happen in this worker so the two views stay
//!      in lock-step.
//!
//! Failure semantics:
//! - Fleet verify fails → skip this tick entirely; the reducer keeps using
//!   its prior cache and `state.verified_fleet` retains the prior snapshot.
//! - Per-channel rollout-manifest verify fails → log + skip that channel
//!   only. The emitted `SignedManifestSet` is partial; `plan_next` simply
//!   doesn't act on missing channels until next poll.
//! - Reducer channel send fails (closed/full) → log + skip the emit. Full
//!   is unusual at 30s cadence; closed means CP is shutting down.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use nixfleet_reconciler::planner_types::SignedManifestSet;
use nixfleet_reconciler::{
    VerifiedFleet, VerifiedRolloutManifest, compute_rollout_id_for_channel, verify_artifact,
    verify_rollout_manifest,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::{ReducerInput, ShutdownToken};
use crate::polling::signed_fetch;
use crate::rollouts_source::RolloutsSource;
use crate::server::{AppState, VerifiedFleetSnapshot};

/// Poll cadence.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Forge URLs + trust path the manifest-poll worker reads on every tick.
/// CLI builds this from `--channel-refs-{artifact,signature}-url`,
/// `--channel-refs-token-file`, `--trust-file`, `--freshness-window-secs`.
///
/// Token + trust files are re-read on every poll so rotation propagates
/// without restart.
#[derive(Debug, Clone)]
pub struct ChannelRefsSource {
    pub artifact_url: String,
    pub signature_url: String,
    pub token_file: Option<PathBuf>,
    pub trust_path: PathBuf,
    pub freshness_window: Duration,
}

pub fn spawn(
    state: Arc<AppState>,
    clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();

        if state.channel_refs_source.is_none() {
            tracing::info!(
                target: "cp_manifest_poll",
                "manifest_poll: no channel_refs_source configured; idling",
            );
            let _ = (&mut shutdown_rx).await;
            return;
        }

        let client = signed_fetch::build_client();
        // First tick fires immediately so the reducer's `manifests` cache
        // is populated before agents can POST HostEvents into the
        // listener. `prime_blocking` (above) populates
        // `state.verified_fleet` for the /v1/fleet.resolved route +
        // dispatch planning, but it does NOT emit `ManifestSetUpdated`
        // to the reducer's input channel: the reducer holds a separate
        // in-memory cache fed only via this worker's emits per
        // RFC-0011 §1 invariant #4 (one MPSC, one mutator per side).
        // Without an immediate first tick, every HostEvent arriving in
        // the warm-up window is dropped at the reducer.
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "cp_manifest_poll",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {}
            }

            match poll_once(&client, &state, &clock).await {
                Ok(Some((set, fleet_snapshot))) => {
                    publish_verified_fleet(&state, fleet_snapshot).await;
                    if let Err(err) = input_tx
                        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
                        .await
                    {
                        tracing::warn!(
                            target: "cp_manifest_poll",
                            error = %err,
                            "manifest_poll: reducer input channel closed",
                        );
                        return;
                    }
                }
                Ok(None) => {
                    // Sources not yet configured (transient race during
                    // startup tests).
                }
                Err(err) => {
                    tracing::warn!(
                        target: "cp_manifest_poll",
                        error = %err,
                        "manifest_poll: tick failed; retaining previous cache",
                    );
                }
            }
        }
    })
}

/// Synchronous startup prime. Called by `server::serve` before the listener
/// opens so routes that read `state.verified_fleet` (e.g. `channel_status`,
/// the enrollment route's freshness check) see a populated snapshot on
/// the first request.
///
/// Returns `Ok(false)` when no `channel_refs_source` is configured (test +
/// offline boot paths fall back to `prime_from_artifact_files`).
pub async fn prime_blocking(state: &Arc<AppState>, clock: &ClockHandle) -> anyhow::Result<bool> {
    if state.channel_refs_source.is_none() {
        return Ok(false);
    }
    let client = signed_fetch::build_client();
    match poll_once(&client, state, clock).await? {
        Some((_set, snapshot)) => {
            publish_verified_fleet(state, snapshot).await;
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn publish_verified_fleet(state: &Arc<AppState>, snapshot: VerifiedFleetSnapshot) {
    *state.verified_fleet.write().await = Some(snapshot);
    state
        .artifact_primed
        .store(true, std::sync::atomic::Ordering::Release);
}

async fn poll_once(
    client: &reqwest::Client,
    state: &Arc<AppState>,
    clock: &ClockHandle,
) -> anyhow::Result<Option<(SignedManifestSet, VerifiedFleetSnapshot)>> {
    let Some(channel_refs_source) = state.channel_refs_source.as_ref() else {
        return Ok(None);
    };

    let now = clock.now();
    let (trusted_keys, reject_before) =
        signed_fetch::read_trust_roots(&channel_refs_source.trust_path, now)?;
    let token = signed_fetch::read_token(channel_refs_source.token_file.as_deref())?;

    // 1. Fetch + verify fleet.resolved.
    let (artifact_bytes, signature_bytes) = signed_fetch::fetch_signed_pair(
        client,
        &channel_refs_source.artifact_url,
        &channel_refs_source.signature_url,
        token.as_deref(),
    )
    .await?;

    let fleet: VerifiedFleet = verify_artifact(
        &artifact_bytes,
        &signature_bytes,
        &trusted_keys,
        now,
        channel_refs_source.freshness_window,
        reject_before,
    )
    .map_err(|e| anyhow::anyhow!("verify_artifact: {e:?}"))?;

    // Content-address anchor for downstream rolloutId derivation. MUST be
    // computed against the received bytes, not a re-serialised parse —
    // additive schema changes the CP's proto doesn't yet know about would
    // shift the anchor relative to what CI signed.
    let fleet_hash = nixfleet_reconciler::canonical_hash_from_bytes(&artifact_bytes)
        .map_err(|e| anyhow::anyhow!("canonical_hash_from_bytes: {e:?}"))?;

    // 2. Per-channel rollout manifest fetch + verify.
    let mut rollouts: HashMap<String, VerifiedRolloutManifest> = HashMap::new();
    if let Some(rollouts_source) = state.rollouts_source.as_ref() {
        for channel in fleet.inner().channels.keys() {
            match fetch_and_verify_channel_manifest(
                rollouts_source,
                fleet.inner(),
                &fleet_hash,
                channel,
                &trusted_keys,
                now,
                channel_refs_source.freshness_window,
                reject_before,
            )
            .await
            {
                Ok(Some(verified)) => {
                    rollouts.insert(channel.clone(), verified);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        target: "cp_manifest_poll",
                        %channel,
                        error = %err,
                        "manifest_poll: rollout manifest fetch/verify failed; \
                         omitting channel from SignedManifestSet (next tick retries)",
                    );
                }
            }
        }
    } else {
        tracing::debug!(
            target: "cp_manifest_poll",
            "manifest_poll: no rollouts_source configured; emitting SignedManifestSet with empty rollouts (plan_next emits no actions)",
        );
    }

    let snapshot = VerifiedFleetSnapshot {
        fleet: Arc::new(fleet.inner().clone()),
        fleet_resolved_hash: fleet_hash,
        artifact_bytes,
        signature_bytes,
    };
    let set = SignedManifestSet::new(fleet, rollouts);
    Ok(Some((set, snapshot)))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_verify_channel_manifest(
    rollouts_source: &RolloutsSource,
    fleet: &nixfleet_proto::FleetResolved,
    fleet_hash: &str,
    channel: &str,
    trusted_keys: &[nixfleet_proto::TrustedPubkey],
    now: chrono::DateTime<chrono::Utc>,
    freshness_window: Duration,
    reject_before: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Option<VerifiedRolloutManifest>> {
    let rollout_id = match compute_rollout_id_for_channel(fleet, fleet_hash, channel)? {
        Some(id) => id,
        None => return Ok(None),
    };
    let (manifest_bytes, signature_bytes) = rollouts_source.fetch_pair(&rollout_id).await?;
    let verified = verify_rollout_manifest(
        &manifest_bytes,
        &signature_bytes,
        trusted_keys,
        now,
        freshness_window,
        reject_before,
    )
    .map_err(|e| anyhow::anyhow!("verify_rollout_manifest({channel}): {e:?}"))?;
    let m = verified.inner();
    check_rollout_id_discriminator(&m.channel, &m.channel_ref, &rollout_id)
        .map_err(|e| anyhow::anyhow!("{channel}: {e}"))?;
    Ok(Some(verified))
}

/// CP-side storage invariant: a signed rollout manifest may only be stored
/// under the canonical RolloutId it claims via its own `(channel,
/// channel_ref)` fields. Catches the bytes-vs-url-claim substitution that
/// the old content-address check used to catch syntactically (under
/// `rollout_id = sha256(bytes)`); under the semantic identifier (RFC-0012
/// §6.3 `{channel}@{channel_ref}`), the equivalent check is parsed-id
/// equality. Mandated by the `verify_rollout_manifest` docstring; mirrors
/// the agent's `assert_rollout_id_matches` on the consumer side.
fn check_rollout_id_discriminator(
    manifest_channel: &str,
    manifest_channel_ref: &str,
    requested: &str,
) -> anyhow::Result<()> {
    let parsed = nixfleet_proto::RolloutId::new(manifest_channel, manifest_channel_ref);
    if parsed.as_str() != requested {
        return Err(anyhow::anyhow!(
            "rollout_id discriminator failed: requested {requested} but signed manifest declares {parsed}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_accepts_canonical_match() {
        check_rollout_id_discriminator("stable", "abc1234deadbeef", "stable@abc1234deadbeef")
            .expect("canonical id matches");
    }

    #[test]
    fn discriminator_rejects_channel_substitution() {
        let err =
            check_rollout_id_discriminator("edge", "abc1234deadbeef", "stable@abc1234deadbeef")
                .expect_err("channel substitution rejected");
        let msg = err.to_string();
        assert!(msg.contains("discriminator failed"), "msg: {msg}");
        assert!(msg.contains("stable@abc1234deadbeef"), "msg: {msg}");
        assert!(msg.contains("edge@abc1234deadbeef"), "msg: {msg}");
    }

    #[test]
    fn discriminator_rejects_ref_substitution() {
        let err =
            check_rollout_id_discriminator("stable", "def5678feedface", "stable@abc1234deadbeef")
                .expect_err("ref substitution rejected");
        let msg = err.to_string();
        assert!(msg.contains("discriminator failed"), "msg: {msg}");
        assert!(msg.contains("stable@abc1234deadbeef"), "msg: {msg}");
        assert!(msg.contains("stable@def5678feedface"), "msg: {msg}");
    }

    #[test]
    fn discriminator_rejects_legacy_hex_only_request() {
        // Regression guard against accidental drift back to a 64-char
        // hex sha256 comparator: the canonical request shape under
        // RFC-0012 §6.3 is `{channel}@{channel_ref}`, not a bare hash.
        let legacy_hex = "a".repeat(64);
        let err = check_rollout_id_discriminator("stable", "abc1234", &legacy_hex)
            .expect_err("legacy hex-only request rejected");
        assert!(err.to_string().contains("discriminator failed"));
    }
}
