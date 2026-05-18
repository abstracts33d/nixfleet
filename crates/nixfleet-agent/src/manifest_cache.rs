//! Rollout manifest fetch + verify + disk cache. Disk-cache hit re-verifies
//! the bytes (defense in depth); miss fetches from CP, verifies, writes
//! through. Per RFC-0005 §4.1, the dispatch path also asserts the manifest's
//! declared `target_closure` for this host matches the dispatched value
//! before the reducer ever sees the event.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use nixfleet_proto::{RolloutId, RolloutManifest, TrustConfig};
use nixfleet_reconciler::{
    VerifiedFleet, VerifiedRolloutManifest, canonical_hash_from_bytes, verify_artifact,
};

/// RFC-0010 §1.5 convention: agent reads trust roots from a hardcoded
/// path, not a CLI flag. Same shape as `/etc/nixfleet/agent/health-checks.json`.
pub const DEFAULT_TRUST_PATH: &str = "/etc/nixfleet/agent/trust.json";

#[derive(Debug)]
pub enum ManifestError {
    Missing(String),
    VerifyFailed(String),
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

/// Production freshness window for signed-artifact verification. Matches
/// CP's channel-refs poll cadence (RFC-0010 §1.5).
pub const DEFAULT_FRESHNESS_WINDOW_SECS: u64 = 3600;

pub struct ManifestCache {
    rollouts_dir: PathBuf,
    fleet_dir: PathBuf,
    trust_path: PathBuf,
    freshness_window: std::time::Duration,
}

impl ManifestCache {
    pub fn new(state_dir: &Path, trust_path: &Path) -> Self {
        Self::new_with_freshness(
            state_dir,
            trust_path,
            std::time::Duration::from_secs(DEFAULT_FRESHNESS_WINDOW_SECS),
        )
    }

    /// Tunable-freshness constructor. Tests with fixed-`signedAt` fixtures
    /// pass a longer window so old signed bytes still verify; production
    /// uses [`Self::new`] which pins [`DEFAULT_FRESHNESS_WINDOW_SECS`].
    pub fn new_with_freshness(
        state_dir: &Path,
        trust_path: &Path,
        freshness_window: std::time::Duration,
    ) -> Self {
        Self {
            rollouts_dir: state_dir.join("rollouts"),
            fleet_dir: state_dir.join("fleet"),
            trust_path: trust_path.to_path_buf(),
            freshness_window,
        }
    }

    /// Construct a `ManifestCache` rooted under `state_dir/{rollouts,fleet}/`
    /// and pointed at the conventional [`DEFAULT_TRUST_PATH`]. The longpoll
    /// worker uses this; tests use [`Self::new`] to inject a tempdir-rooted
    /// trust file.
    pub fn new_default(state_dir: &Path) -> Self {
        Self::new(state_dir, Path::new(DEFAULT_TRUST_PATH))
    }

    fn manifest_path(&self, rollout_id: &str) -> PathBuf {
        self.rollouts_dir.join(format!("{rollout_id}.json"))
    }

    fn signature_path(&self, rollout_id: &str) -> PathBuf {
        self.rollouts_dir.join(format!("{rollout_id}.json.sig"))
    }

    fn fleet_path(&self) -> PathBuf {
        self.fleet_dir.join("fleet.resolved.json")
    }

    fn fleet_sig_path(&self) -> PathBuf {
        self.fleet_dir.join("fleet.resolved.json.sig")
    }

    /// Reads (manifest, sig) bytes if both exist; does NOT verify.
    pub fn read_cached_bytes(&self, rollout_id: &str) -> Option<(Vec<u8>, Vec<u8>)> {
        let manifest = std::fs::read(self.manifest_path(rollout_id)).ok()?;
        let sig = std::fs::read(self.signature_path(rollout_id)).ok()?;
        Some((manifest, sig))
    }

    fn load_trust_roots(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<(
        Vec<nixfleet_proto::TrustedPubkey>,
        Option<chrono::DateTime<Utc>>,
    )> {
        let raw = std::fs::read_to_string(&self.trust_path)
            .with_context(|| format!("read trust file {}", self.trust_path.display()))?;
        let trust: TrustConfig = serde_json::from_str(&raw).context("parse trust file")?;
        Ok((
            trust.ci_release_key.active_keys_at(now),
            trust.ci_release_key.reject_before,
        ))
    }

    /// Path-traversal sanity check on the rollout_id string before it is
    /// embedded in any filesystem path. Mirrors the CP route validator's
    /// shape (RFC-0008 §6.3 canonical format `"channel@channel_ref"`); both
    /// layers refuse `/` and `..` so neither side can be coerced into
    /// reading a file outside its rollouts directory.
    fn validate_rollout_id_for_path(rollout_id: &str) -> Result<(), ManifestError> {
        if rollout_id.contains('/') || rollout_id.contains("..") {
            return Err(ManifestError::Mismatch(format!(
                "rollout_id {rollout_id:?} contains path-traversal characters"
            )));
        }
        Ok(())
    }

    fn verify_bytes(
        &self,
        manifest_bytes: &[u8],
        signature_bytes: &[u8],
        advertised_rollout_id: &str,
    ) -> Result<VerifiedRolloutManifest, ManifestError> {
        // Window comes from `ManifestCache::freshness_window` — production
        // uses `DEFAULT_FRESHNESS_WINDOW_SECS` (1h, matching channel-refs
        // poll posture); test harnesses with fixed-`signedAt` fixtures
        // override via `new_with_freshness`.
        let now = Utc::now();
        let (trusted_keys, reject_before) = self
            .load_trust_roots(now)
            .map_err(|err| ManifestError::VerifyFailed(format!("load trust roots: {err:#}")))?;
        let window = self.freshness_window;
        let verified = nixfleet_reconciler::verify_rollout_manifest(
            manifest_bytes,
            signature_bytes,
            &trusted_keys,
            now,
            window,
            reject_before,
        )
        .map_err(|err| ManifestError::VerifyFailed(format!("{err:?}")))?;

        Self::assert_rollout_id_matches(verified.inner(), advertised_rollout_id)?;
        Ok(verified)
    }

    /// Discriminator per RFC-0008 §6.3: the canonical identity is
    /// `"{channel}@{channel_ref}"` derived from the parsed manifest's
    /// fields. Defense-in-depth that the advertised id matches the
    /// manifest's actual identity; the signature verify above already
    /// authenticates the bytes, so this catches filename / advertised-id
    /// substitution attacks where attacker-signed bytes carrying a
    /// different `(channel, channel_ref)` arrive at a path claiming the
    /// canonical id of a different rollout.
    fn assert_rollout_id_matches(
        manifest: &RolloutManifest,
        advertised_rollout_id: &str,
    ) -> Result<(), ManifestError> {
        let parsed = RolloutId::new(&manifest.channel, &manifest.channel_ref);
        if parsed.as_str() != advertised_rollout_id {
            return Err(ManifestError::Mismatch(format!(
                "advertised rolloutId {advertised} != parsed RolloutId {parsed}",
                advertised = advertised_rollout_id,
                parsed = parsed.as_str(),
            )));
        }
        Ok(())
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

    /// RFC-0005 §4.1 advisory-payload contract: agent acts on a Dispatch
    /// only if the dispatched `target_closure` matches the manifest's
    /// declared `target_closure` for this host. Pure function; tested in
    /// isolation. The dispatch path's canonical entry composes this with
    /// [`fetch_or_load`] via [`ensure_for_dispatch`].
    fn assert_target_closure(
        manifest: &RolloutManifest,
        hostname: &str,
        expected_target_closure: &str,
    ) -> Result<(), ManifestError> {
        let host = manifest
            .host_set
            .iter()
            .find(|h| h.hostname == hostname)
            .ok_or_else(|| {
                ManifestError::Mismatch(format!("hostname {hostname:?} not in manifest.host_set"))
            })?;
        if host.target_closure != expected_target_closure {
            return Err(ManifestError::Mismatch(format!(
                "dispatch target_closure {dispatched:?} != manifest target_closure {manifest_value:?} for hostname {hostname:?}",
                dispatched = expected_target_closure,
                manifest_value = host.target_closure,
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

    /// Disk-cache hit re-verifies bytes (defense in depth); miss OR cache
    /// verify-failure fetches from CP, verifies, writes through. Public so
    /// the periodic `manifest_poll` worker can fetch rollouts independently
    /// of any dispatch arrival (agent-side feed).
    ///
    /// LOADBEARING: verify-failure falls through to fetch. Returning the
    /// verify error directly would leave a stale cached manifest
    /// permanently stuck on freshness/signature errors without ever
    /// attempting a CP refresh.
    pub async fn fetch_or_load(
        &self,
        client: &reqwest::Client,
        cp_url: &str,
        rollout_id: &str,
    ) -> Result<VerifiedRolloutManifest, ManifestError> {
        Self::validate_rollout_id_for_path(rollout_id)?;

        if let Some((manifest_bytes, sig_bytes)) = self.read_cached_bytes(rollout_id) {
            match self.verify_bytes(&manifest_bytes, &sig_bytes, rollout_id) {
                Ok(verified) => return Ok(verified),
                Err(err) => {
                    tracing::info!(
                        target: "agent_manifest_cache",
                        rollout_id = %rollout_id,
                        error = %err.reason(),
                        "cached rollout manifest failed verification; falling through to fetch",
                    );
                    // fall through
                }
            }
        }

        let base = cp_url.trim_end_matches('/');
        let manifest_url = format!("{base}/v1/rollouts/{rollout_id}");
        let sig_url = format!("{base}/v1/rollouts/{rollout_id}/sig");

        let manifest_bytes = fetch(client, &manifest_url).await?;
        let sig_bytes = fetch(client, &sig_url).await?;

        let verified = self.verify_bytes(&manifest_bytes, &sig_bytes, rollout_id)?;

        if let Err(err) = self.write_cache(rollout_id, &manifest_bytes, &sig_bytes) {
            tracing::warn!(
                rollout_id = %rollout_id,
                error = %err,
                "manifest cache: write-through failed (will refetch next checkin)",
            );
        }

        Ok(verified)
    }

    /// Fetch + verify a manifest, then assert `(hostname, wave_index)`
    /// membership. Used by callers that need the explicit wave-index
    /// sanity check; the dispatch path uses [`Self::ensure_for_dispatch`]
    /// instead.
    pub async fn ensure(
        &self,
        client: &reqwest::Client,
        cp_url: &str,
        rollout_id: &str,
        hostname: &str,
        wave_index: u32,
    ) -> Result<VerifiedRolloutManifest, ManifestError> {
        let verified = self.fetch_or_load(client, cp_url, rollout_id).await?;
        Self::assert_membership(verified.inner(), hostname, wave_index)?;
        Ok(verified)
    }

    /// Canonical dispatch entry: fetch + verify the manifest, then assert
    /// the dispatched `target_closure` matches the manifest's declaration
    /// for this host (RFC-0005 §4.1). The longpoll worker's only path
    /// from a `DispatchResponse` into the reducer.
    pub async fn ensure_for_dispatch(
        &self,
        client: &reqwest::Client,
        cp_url: &str,
        rollout_id: &str,
        hostname: &str,
        expected_target_closure: &str,
    ) -> Result<VerifiedRolloutManifest, ManifestError> {
        let verified = self.fetch_or_load(client, cp_url, rollout_id).await?;
        Self::assert_target_closure(verified.inner(), hostname, expected_target_closure)?;
        Ok(verified)
    }

    fn read_cached_fleet_bytes(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let artifact = std::fs::read(self.fleet_path()).ok()?;
        let sig = std::fs::read(self.fleet_sig_path()).ok()?;
        Some((artifact, sig))
    }

    fn write_fleet_cache(&self, artifact_bytes: &[u8], sig_bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.fleet_dir)
            .with_context(|| format!("create fleet cache dir {}", self.fleet_dir.display()))?;
        std::fs::write(self.fleet_path(), artifact_bytes)
            .with_context(|| format!("write {}", self.fleet_path().display()))?;
        std::fs::write(self.fleet_sig_path(), sig_bytes)
            .with_context(|| format!("write {}", self.fleet_sig_path().display()))?;
        Ok(())
    }

    fn verify_fleet_bytes(
        &self,
        artifact_bytes: &[u8],
        signature_bytes: &[u8],
    ) -> Result<VerifiedFleet, ManifestError> {
        let now = Utc::now();
        let (trusted_keys, reject_before) = self
            .load_trust_roots(now)
            .map_err(|err| ManifestError::VerifyFailed(format!("load trust roots: {err:#}")))?;
        let window = self.freshness_window;
        verify_artifact(
            artifact_bytes,
            signature_bytes,
            &trusted_keys,
            now,
            window,
            reject_before,
        )
        .map_err(|err| ManifestError::VerifyFailed(format!("{err:?}")))
    }

    /// Disk-cache hit re-verifies bytes; miss OR cache verify-failure
    /// fetches `/v1/fleet.resolved` + `/sig` from CP, verifies, writes
    /// through. Returns the verified struct paired with
    /// `canonical_hash_from_bytes(artifact_bytes)` so the periodic
    /// `manifest_poll` worker can cross-check each fetched rollout's
    /// `fleet_resolved_hash` against this anchor (per the architect's
    /// amendment to the d010-feed plan, restated under Option C:
    /// discriminator moves from this function's signature into the
    /// worker's tick logic as a cross-consistency check between the
    /// two signed sources).
    ///
    /// LOADBEARING: verify-failure falls through to fetch. Returning
    /// the cache's verify Err without attempting CP refresh would
    /// leave an aged-out cached manifest permanently stuck — every
    /// `manifest_poll` tick re-verifying the same stale bytes,
    /// returning `Stale`, and the reducer's `advance_tick` pass-gate
    /// would have no fresh manifest to consult.
    pub async fn fetch_or_load_fleet(
        &self,
        client: &reqwest::Client,
        cp_url: &str,
    ) -> Result<(VerifiedFleet, String), ManifestError> {
        if let Some((artifact_bytes, sig_bytes)) = self.read_cached_fleet_bytes() {
            match self.verify_fleet_bytes(&artifact_bytes, &sig_bytes) {
                Ok(verified) => {
                    let hash = canonical_hash_from_bytes(&artifact_bytes).map_err(|err| {
                        ManifestError::Mismatch(format!("hash cached fleet: {err:?}"))
                    })?;
                    return Ok((verified, hash));
                }
                Err(err) => {
                    tracing::info!(
                        target: "agent_manifest_cache",
                        error = %err.reason(),
                        "cached fleet manifest failed verification; falling through to fetch",
                    );
                    // fall through
                }
            }
        }

        let base = cp_url.trim_end_matches('/');
        let artifact_url = format!("{base}/v1/fleet.resolved");
        let sig_url = format!("{base}/v1/fleet.resolved/sig");

        let artifact_bytes = fetch(client, &artifact_url).await?;
        let sig_bytes = fetch(client, &sig_url).await?;

        let verified = self.verify_fleet_bytes(&artifact_bytes, &sig_bytes)?;
        let hash = canonical_hash_from_bytes(&artifact_bytes)
            .map_err(|err| ManifestError::Mismatch(format!("hash fetched fleet: {err:?}")))?;

        if let Err(err) = self.write_fleet_cache(&artifact_bytes, &sig_bytes) {
            tracing::warn!(
                error = %err,
                "fleet cache: write-through failed (will refetch next tick)",
            );
        }

        Ok((verified, hash))
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
    use nixfleet_proto::fleet_resolved::{HealthGate, Meta};
    use nixfleet_proto::rollout_manifest::HostWave;

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

    fn manifest_with(host_set: Vec<HostWave>) -> RolloutManifest {
        RolloutManifest {
            schema_version: 1,
            display_name: "stable@abc1234".into(),
            channel: "stable".into(),
            channel_ref: "abc1234deadbeef".into(),
            fleet_resolved_hash: "1111111111111111111111111111111111111111111111111111111111111111"
                .into(),
            host_set,
            health_gate: HealthGate::default(),
            disruption_budgets: Vec::new(),
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: None,
            },
        }
    }

    fn host_wave(hostname: &str, wave_index: u32, target_closure: &str) -> HostWave {
        HostWave {
            hostname: hostname.into(),
            wave_index,
            target_closure: target_closure.into(),
        }
    }

    #[test]
    fn assert_target_closure_passes_on_match() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        ManifestCache::assert_target_closure(&m, "h1", "closure-A").expect("match");
    }

    #[test]
    fn assert_target_closure_fails_on_target_mismatch() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        let err = ManifestCache::assert_target_closure(&m, "h1", "closure-B")
            .expect_err("target mismatch");
        let msg = err.reason();
        assert!(
            msg.contains("closure-A"),
            "expected manifest target in error: {msg}"
        );
        assert!(
            msg.contains("closure-B"),
            "expected dispatched target in error: {msg}"
        );
    }

    #[test]
    fn assert_target_closure_fails_when_hostname_not_in_set() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        let err = ManifestCache::assert_target_closure(&m, "h2", "closure-A")
            .expect_err("hostname not in set");
        let msg = err.reason();
        assert!(msg.contains("h2"), "expected hostname in error: {msg}");
    }

    #[test]
    fn validate_rollout_id_for_path_refuses_traversal() {
        assert!(ManifestCache::validate_rollout_id_for_path("stable@abc1234").is_ok());
        assert!(ManifestCache::validate_rollout_id_for_path("stable@abc/123").is_err());
        assert!(ManifestCache::validate_rollout_id_for_path("../../../etc/passwd").is_err());
        assert!(ManifestCache::validate_rollout_id_for_path("a..b").is_err());
    }

    #[test]
    fn assert_rollout_id_matches_accepts_canonical_format() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        // manifest_with sets channel="stable", channel_ref="abc1234deadbeef".
        ManifestCache::assert_rollout_id_matches(&m, "stable@abc1234deadbeef")
            .expect("canonical id matches");
    }

    #[test]
    fn assert_rollout_id_matches_rejects_channel_only() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        let err = ManifestCache::assert_rollout_id_matches(&m, "stable")
            .expect_err("channel-only rejected");
        assert!(matches!(err, ManifestError::Mismatch(_)));
    }

    #[test]
    fn assert_rollout_id_matches_rejects_channel_ref_only() {
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        let err = ManifestCache::assert_rollout_id_matches(&m, "abc1234deadbeef")
            .expect_err("channel_ref-only rejected");
        assert!(matches!(err, ManifestError::Mismatch(_)));
    }

    #[test]
    fn fleet_path_and_sig_path_under_fleet_subdir() {
        let state_dir = std::path::PathBuf::from("/tmp/nixfleet-agent-test");
        let cache = ManifestCache::new(&state_dir, std::path::Path::new("/dev/null"));
        assert_eq!(
            cache.fleet_path(),
            state_dir.join("fleet").join("fleet.resolved.json")
        );
        assert_eq!(
            cache.fleet_sig_path(),
            state_dir.join("fleet").join("fleet.resolved.json.sig")
        );
    }

    #[test]
    fn assert_rollout_id_matches_rejects_sha256_hash_format() {
        // Regression guard: a 64-char hex string is what a refactor
        // accidentally reverting the discriminator to a content-hash
        // would produce. RFC-0008 §6.3 specifies the canonical RolloutId
        // is the composite `"channel@channel_ref"`; this test locks the
        // discriminator against drift back to a hash comparator.
        let m = manifest_with(vec![host_wave("h1", 0, "closure-A")]);
        let plausible_hash = "a".repeat(64);
        let err = ManifestCache::assert_rollout_id_matches(&m, &plausible_hash)
            .expect_err("sha256 hex rejected");
        assert!(matches!(err, ManifestError::Mismatch(_)));
    }

    /// Minimal TrustConfig JSON for the cache-fallthrough tests below. A
    /// real ed25519 public key (well-formed but not the producer key) ensures
    /// `load_trust_roots` succeeds; signature verification will fail with
    /// `BadSignature` against arbitrary cache contents — exactly the
    /// fall-through condition we want to exercise.
    fn minimal_trust_json() -> String {
        // base64(32 zero bytes) is a syntactically-valid ed25519 pubkey.
        // No artifact will verify against it; that's the point.
        let zero_pub_b64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        format!(
            r#"{{
                "schemaVersion": 1,
                "ciReleaseKey": {{
                    "current": {{
                        "algorithm": "ed25519",
                        "public": "{zero_pub_b64}"
                    }}
                }}
            }}"#
        )
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    #[test]
    fn fleet_cache_verify_failure_falls_through_to_fetch() {
        // Regression guard for the cache-hit fall-through contract.
        // `fetch_or_load_fleet` must NOT propagate
        // `verify_fleet_bytes` Err directly from the cache-hit branch;
        // it must fall through to the fetch path so an aged-out or
        // corrupt cached manifest can be replaced. Test discriminates
        // by error variant: a regression returns `VerifyFailed` from
        // the cache; correct behavior returns `Missing` from the
        // (unreachable-in-test) CP fetch.
        let dir = tempfile::tempdir().expect("tempdir");
        let trust_path = dir.path().join("trust.json");
        std::fs::write(&trust_path, minimal_trust_json()).expect("write trust");
        std::fs::create_dir_all(dir.path().join("fleet")).expect("mkdir fleet");
        // Garbage bytes — any verify must fail.
        std::fs::write(
            dir.path().join("fleet/fleet.resolved.json"),
            br#"{"schemaVersion":1,"signedAt":"2020-01-01T00:00:00Z"}"#,
        )
        .expect("write cached artifact");
        std::fs::write(dir.path().join("fleet/fleet.resolved.json.sig"), b"sig")
            .expect("write cached sig");

        let cache = ManifestCache::new(dir.path(), &trust_path);
        // 127.0.0.1:1 is guaranteed-unreachable (privileged port + no listener).
        let unreachable_cp = "http://127.0.0.1:1";
        let client = reqwest::Client::new();

        let err = rt()
            .block_on(cache.fetch_or_load_fleet(&client, unreachable_cp))
            .expect_err("fetch_or_load_fleet must error when both cache and CP fail");
        assert!(
            matches!(err, ManifestError::Missing(_)),
            "post-fix MUST fall through to fetch when cache verify fails; \
             error variant indicates which path returned. Got: {err:?}",
        );
    }

    #[test]
    fn rollout_manifest_cache_verify_failure_falls_through_to_fetch() {
        // Parallel regression guard to the fleet-cache fall-through
        // test above — the rollout-manifest `fetch_or_load` must
        // share the same cache-then-fetch discipline.
        let dir = tempfile::tempdir().expect("tempdir");
        let trust_path = dir.path().join("trust.json");
        std::fs::write(&trust_path, minimal_trust_json()).expect("write trust");
        let rollout_id = "stable@abc1234deadbeef";
        std::fs::create_dir_all(dir.path().join("rollouts")).expect("mkdir rollouts");
        std::fs::write(
            dir.path().join(format!("rollouts/{rollout_id}.json")),
            br#"{"schemaVersion":1,"channel":"stable","channelRef":"abc1234deadbeef"}"#,
        )
        .expect("write cached manifest");
        std::fs::write(
            dir.path().join(format!("rollouts/{rollout_id}.json.sig")),
            b"sig",
        )
        .expect("write cached sig");

        let cache = ManifestCache::new(dir.path(), &trust_path);
        let unreachable_cp = "http://127.0.0.1:1";
        let client = reqwest::Client::new();

        let err = rt()
            .block_on(cache.fetch_or_load(&client, unreachable_cp, rollout_id))
            .expect_err("fetch_or_load must error when both cache and CP fail");
        assert!(
            matches!(err, ManifestError::Missing(_)),
            "post-fix MUST fall through to fetch when cache verify fails; \
             error variant indicates which path returned. Got: {err:?}",
        );
    }
}
