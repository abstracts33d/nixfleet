//! Revocations poll loop.
//!
//! Fetches a signed `revocations.json` artifact from a configured
//! URL pair, runs `nixfleet_reconciler::verify_revocations`
//! against the same `ciReleaseKey` trust roots that
//! `channel_refs_poll` uses, and replays the verified list into
//! `cert_revocations` so revocations survive CP rebuilds.
//!
//! Failure semantics: log warn + retain previous DB state. Same
//! posture as `channel_refs_poll` — a transient outage or a bad
//! signature must not blank out a previously-good revocation set.
//!
//! Source-agnostic by design: the CP only knows how to issue
//! `GET <url>` with a Bearer token; concrete URL shapes
//! (Forgejo / GitHub / GitLab raw) are constructed by the
//! consumer alongside the existing channel-refs URLs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::db::Db;
use crate::polling::signed_fetch;

/// Poll cadence — same default as channel-refs poll. Fast enough
/// that operator-declared revocations propagate within a minute;
/// slow enough that the upstream isn't hammered.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Configuration for the revocations poll task. Source-agnostic;
/// the consumer supplies fully-formed URLs that yield raw bytes
/// when GET'd with the configured Bearer token. Trust roots come
/// from the same `trust.json` the channel-refs poll reads, so
/// rotating `nixfleet.trust.ciReleaseKey.current` automatically
/// covers both artifacts.
#[derive(Debug, Clone)]
pub struct RevocationsSource {
    pub artifact_url: String,
    pub signature_url: String,
    pub token_file: Option<PathBuf>,
    pub trust_path: PathBuf,
    pub freshness_window: Duration,
}

/// Spawn the poll task. Runs forever; logs warnings on failure;
/// upserts every verified revocation into `cert_revocations` on
/// success. The DB upsert is idempotent (`revoke_cert` already
/// handles `ON CONFLICT DO UPDATE`), so re-replaying the same
/// signed artifact every minute is a quiet no-op.
pub fn spawn(db: Arc<Db>, config: RevocationsSource) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = signed_fetch::build_client();

        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match poll_once(&client, &config).await {
                Ok(revs) => {
                    let n = revs.revocations.len();
                    let mut applied = 0usize;
                    for entry in &revs.revocations {
                        match db.revocations().revoke_cert(
                            &entry.hostname,
                            entry.not_before,
                            entry.reason.as_deref(),
                            entry.revoked_by.as_deref(),
                        ) {
                            Ok(()) => applied += 1,
                            Err(err) => tracing::warn!(
                                hostname = %entry.hostname,
                                error = %err,
                                "revocations poll: revoke_cert failed for entry",
                            ),
                        }
                    }
                    // — heartbeat at INFO on every successful
                    // tick (not just when applied > 0). Operators tailing
                    // the journal need a positive signal that the poll
                    // is alive; cross-checking forgejo access logs to
                    // prove cadence isn't sustainable. One INFO line per
                    // 60s tick is cheap and load-bearing for ops.
                    tracing::info!(
                        target: "revocations",
                        entries = n,
                        applied = applied,
                        signed_at = ?revs.meta.signed_at,
                        ci_commit = ?revs.meta.ci_commit,
                        "revocations poll: list verified",
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "revocations poll failed; retaining previous cert_revocations state",
                    );
                }
            }
        }
    })
}

async fn poll_once(
    client: &reqwest::Client,
    config: &RevocationsSource,
) -> Result<nixfleet_proto::Revocations> {
    let token = signed_fetch::read_token(config.token_file.as_deref())?;
    let (artifact_bytes, signature_bytes) = signed_fetch::fetch_signed_pair(
        client,
        &config.artifact_url,
        &config.signature_url,
        token.as_deref(),
    )
    .await?;

    let (trusted_keys, reject_before) = signed_fetch::read_trust_roots(&config.trust_path)?;

    nixfleet_reconciler::verify_revocations(
        &artifact_bytes,
        &signature_bytes,
        &trusted_keys,
        chrono::Utc::now(),
        config.freshness_window,
        reject_before,
    )
    .map_err(|e| anyhow::anyhow!("verify_revocations (revocations poll): {e:?}"))
}
