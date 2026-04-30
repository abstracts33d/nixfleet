//! Agent-side rollout manifest fetch + verify + cache + enforce
//! (RFC-0002 §4.4, RFC-0003 §4.1).
//!
//! Before the agent consumes any field of a `target` advertised by
//! the CP, it MUST obtain a manifest matching the advertised
//! `rolloutId` that:
//!   1. Verifies under the trust roots the agent already holds
//!      (`ciReleaseKey` from `trust.json`).
//!   2. Has a recomputed content hash equal to the advertised
//!      `rolloutId` (the partition-attack defense).
//!   3. Lists `(hostname, wave_index)` in `host_set`.
//!
//! Failure modes map 1:1 onto signed `ReportEvent`s:
//!   - Missing → 404 from CP, IO failure, or parse failure.
//!   - VerifyFailed → signature failed against trust roots.
//!   - Mismatch → hash recompute failed, host_set membership failed,
//!                or a previously-cached rolloutId resolves to
//!                different bytes than today's fetch.
//!
//! Cache layout: `<state_dir>/rollouts/<rolloutId>.{json,sig}`.
//! On every checkin the agent compares the advertised rolloutId to
//! its cache. A cache hit is fast (no fetch). A cache miss triggers
//! the full fetch+verify pipeline.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use nixfleet_proto::{RolloutManifest, TrustConfig};

#[derive(Debug)]
pub enum ManifestError {
    /// Manifest could not be obtained at all: 404, network error,
    /// IO failure, or bytes that don't parse as a `RolloutManifest`.
    Missing(String),
    /// Bytes parse but signature verification failed against the
    /// agent's trust roots (`ciReleaseKey`). Same trust class as a
    /// tampered `fleet.resolved.json`.
    VerifyFailed(String),
    /// Cryptographic checks passed, but the content does not match
    /// what the CP claimed: recomputed hash ≠ advertised rolloutId,
    /// `(hostname, wave_index)` not in `host_set`, or this rolloutId
    /// previously resolved to different bytes (cache divergence).
    Mismatch(String),
}

impl ManifestError {
    pub fn reason(&self) -> &str {
        match self {
            ManifestError::Missing(s) => s,
            ManifestError::VerifyFailed(s) => s,
            ManifestError::Mismatch(s) => s,
        }
    }
}

pub struct ManifestCache {
    rollouts_dir: PathBuf,
    trust_path: PathBuf,
}

impl ManifestCache {
    /// `state_dir` is the agent's `--state-dir`; the cache lives
    /// under `<state_dir>/rollouts/`. `trust_path` is the agent's
    /// `--trust-file`.
    pub fn new(state_dir: &Path, trust_path: &Path) -> Self {
        Self {
            rollouts_dir: state_dir.join("rollouts"),
            trust_path: trust_path.to_path_buf(),
        }
    }

    fn manifest_path(&self, rollout_id: &str) -> PathBuf {
        self.rollouts_dir.join(format!("{rollout_id}.json"))
    }

    fn signature_path(&self, rollout_id: &str) -> PathBuf {
        self.rollouts_dir.join(format!("{rollout_id}.json.sig"))
    }

    /// Read the cached pair from disk if it exists. Does NOT verify;
    /// caller is expected to either trust the cache (after the first
    /// verify it passed in this same call's predecessor) or run a
    /// re-verify if defense-in-depth on disk corruption matters.
    /// Returns `None` if either file is missing.
    pub fn read_cached_bytes(&self, rollout_id: &str) -> Option<(Vec<u8>, Vec<u8>)> {
        let manifest = std::fs::read(self.manifest_path(rollout_id)).ok()?;
        let sig = std::fs::read(self.signature_path(rollout_id)).ok()?;
        Some((manifest, sig))
    }

    fn load_trust_roots(
        &self,
    ) -> Result<(Vec<nixfleet_proto::TrustedPubkey>, Option<chrono::DateTime<Utc>>)> {
        let raw = std::fs::read_to_string(&self.trust_path)
            .with_context(|| format!("read trust file {}", self.trust_path.display()))?;
        let trust: TrustConfig =
            serde_json::from_str(&raw).context("parse trust file")?;
        Ok((
            trust.ci_release_key.active_keys(),
            trust.ci_release_key.reject_before,
        ))
    }

    fn verify_bytes(
        &self,
        manifest_bytes: &[u8],
        signature_bytes: &[u8],
        advertised_rollout_id: &str,
    ) -> Result<RolloutManifest, ManifestError> {
        let (trusted_keys, reject_before) = self.load_trust_roots().map_err(|err| {
            ManifestError::VerifyFailed(format!("load trust roots: {err:#}"))
        })?;
        // 1h freshness window: same posture as the channel-refs poll.
        let now = Utc::now();
        let window = std::time::Duration::from_secs(3600);
        let manifest = nixfleet_reconciler::verify_rollout_manifest(
            manifest_bytes,
            signature_bytes,
            &trusted_keys,
            now,
            window,
            reject_before,
        )
        .map_err(|err| ManifestError::VerifyFailed(format!("{err:?}")))?;

        let recomputed = nixfleet_reconciler::compute_rollout_id(&manifest)
            .map_err(|err| ManifestError::Mismatch(format!("compute_rollout_id: {err:?}")))?;
        if recomputed != advertised_rollout_id {
            return Err(ManifestError::Mismatch(format!(
                "advertised rolloutId {advertised} ≠ recomputed sha256 {recomputed}",
                advertised = advertised_rollout_id
            )));
        }
        Ok(manifest)
    }

    fn assert_membership(
        manifest: &RolloutManifest,
        hostname: &str,
        wave_index: u32,
    ) -> Result<(), ManifestError> {
        let in_set = manifest
            .host_set
            .iter()
            .any(|h| h.hostname == hostname && h.wave_index == wave_index);
        if !in_set {
            return Err(ManifestError::Mismatch(format!(
                "(hostname={hostname}, wave_index={wave_index}) not in manifest.host_set"
            )));
        }
        Ok(())
    }

    fn write_cache(&self, rollout_id: &str, manifest_bytes: &[u8], sig_bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.rollouts_dir).with_context(|| {
            format!("create rollouts cache dir {}", self.rollouts_dir.display())
        })?;
        std::fs::write(self.manifest_path(rollout_id), manifest_bytes)
            .with_context(|| format!("write {}", self.manifest_path(rollout_id).display()))?;
        std::fs::write(self.signature_path(rollout_id), sig_bytes)
            .with_context(|| format!("write {}", self.signature_path(rollout_id).display()))?;
        Ok(())
    }

    /// Ensure a verified manifest is available for `rollout_id`. On
    /// cache hit, validates cached bytes against the advertised id +
    /// host_set membership. On cache miss, fetches from the CP,
    /// verifies, and writes through to disk.
    pub async fn ensure(
        &self,
        client: &reqwest::Client,
        cp_url: &str,
        rollout_id: &str,
        hostname: &str,
        wave_index: u32,
    ) -> Result<RolloutManifest, ManifestError> {
        // Cache hit: re-verify the cached pair (defense in depth
        // against state-dir tamper) and check membership.
        if let Some((manifest_bytes, sig_bytes)) = self.read_cached_bytes(rollout_id) {
            let manifest = self.verify_bytes(&manifest_bytes, &sig_bytes, rollout_id)?;
            Self::assert_membership(&manifest, hostname, wave_index)?;
            return Ok(manifest);
        }

        // Cache miss: fetch + verify + write through.
        let base = cp_url.trim_end_matches('/');
        let manifest_url = format!("{base}/v1/rollouts/{rollout_id}");
        let sig_url = format!("{base}/v1/rollouts/{rollout_id}/sig");

        let manifest_bytes = fetch(client, &manifest_url).await?;
        let sig_bytes = fetch(client, &sig_url).await?;

        let manifest = self.verify_bytes(&manifest_bytes, &sig_bytes, rollout_id)?;
        Self::assert_membership(&manifest, hostname, wave_index)?;

        if let Err(err) = self.write_cache(rollout_id, &manifest_bytes, &sig_bytes) {
            // Cache-write failure is non-fatal — the agent still has
            // a verified in-memory manifest for this checkin. Next
            // checkin will refetch. Log warn rather than error.
            tracing::warn!(
                rollout_id = %rollout_id,
                error = %err,
                "manifest cache: write-through failed (will refetch next checkin)",
            );
        }

        Ok(manifest)
    }
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, ManifestError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|err| ManifestError::Missing(format!("GET {url}: {err}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(ManifestError::Missing(format!("404 from {url}")));
    }
    if !status.is_success() {
        return Err(ManifestError::Missing(format!("{url}: {status}")));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|err| ManifestError::Missing(format!("read body {url}: {err}")))?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_error_variants_distinct_on_debug() {
        let outcomes = [
            format!("{:?}", ManifestError::Missing("x".into())),
            format!("{:?}", ManifestError::VerifyFailed("x".into())),
            format!("{:?}", ManifestError::Mismatch("x".into())),
        ];
        let unique: std::collections::HashSet<_> = outcomes.iter().collect();
        assert_eq!(unique.len(), outcomes.len());
    }
}
