//! HTTP-fetched rollout-manifest source.
//!
//! Companion to `--rollouts-dir` (filesystem path) for fleets whose
//! CP can't include the manifests in `inputs.self` at closure-build
//! time — which is every fleet with a normal release pipeline, since
//! `nixfleet-release` writes `releases/rollouts/` AFTER building the
//! closures it just signed. The first activation of any new closure
//! therefore points `--rollouts-dir` at a path inside its own source
//! tree that doesn't yet contain the manifests.
//!
//! This module breaks the bootstrap by mirroring the same
//! HTTP-polling pattern `channel_refs_poll` and `revocations_poll`
//! already use: fetch from a configured URL pair, verify the
//! content-addressed hash matches the path, hand the bytes to the
//! handler.
//!
//! Trust: signature verification stays in the agent, not the CP, per
//! the existing handler's contract ("the CP holds NO signing key for
//! rollouts"). The CP only checks `sha256(manifest) == rolloutId`,
//! same content-addressing invariant as the filesystem path. The
//! agent verifies the signature against `ciReleaseKey` on receipt.
//!
//! Fetch posture: on-demand (no background poll). Manifests are
//! immutable (rolloutId IS the content hash), so once a fetch
//! succeeds the bytes are valid forever. Caching can be added later
//! if Forgejo round-trips become a hotspot — for now the agent only
//! fetches on dispatch, which is rare enough that one round-trip is
//! cheap.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use crate::signed_fetch;

/// URL template substitution token. Both `artifact_url_template` and
/// `signature_url_template` must contain this literal — the CP errors
/// at startup otherwise. Keeping the marker explicit (rather than
/// `{}` or printf-style) makes it readable in operator configs and
/// avoids ambiguity with URL fragments that legitimately contain
/// braces.
pub const ROLLOUT_ID_PLACEHOLDER: &str = "{rolloutId}";

/// Configuration for HTTP-fetched rollout manifests. Source-agnostic:
/// any URL pair that yields the raw manifest + signature bytes when
/// GET'd with the configured Bearer token works (Forgejo `/raw/...`,
/// GitHub raw, GitLab `/-/raw/...`, plain HTTP).
#[derive(Debug, Clone)]
pub struct RolloutsSource {
    /// URL template for the manifest. Must contain `{rolloutId}`.
    /// Example: `https://forgejo.example.com/owner/fleet/raw/branch/main/releases/rollouts/{rolloutId}.json`
    pub artifact_url_template: String,
    /// URL template for the signature. Must contain `{rolloutId}`.
    pub signature_url_template: String,
    /// Optional Bearer token file. None → unauthenticated GET.
    pub token_file: Option<PathBuf>,
    /// Per-request HTTP timeout. Independent from the polling clients
    /// because rollout fetches are user-driven (an agent is waiting).
    pub timeout: Duration,
}

impl RolloutsSource {
    /// Build a source after validating the templates contain the
    /// placeholder. Call once at startup; bail if either template is
    /// malformed.
    pub fn new(
        artifact_url_template: String,
        signature_url_template: String,
        token_file: Option<PathBuf>,
    ) -> Result<Self> {
        if !artifact_url_template.contains(ROLLOUT_ID_PLACEHOLDER) {
            return Err(anyhow!(
                "rollouts source: artifact_url_template must contain {ROLLOUT_ID_PLACEHOLDER}"
            ));
        }
        if !signature_url_template.contains(ROLLOUT_ID_PLACEHOLDER) {
            return Err(anyhow!(
                "rollouts source: signature_url_template must contain {ROLLOUT_ID_PLACEHOLDER}"
            ));
        }
        Ok(Self {
            artifact_url_template,
            signature_url_template,
            token_file,
            timeout: Duration::from_secs(15),
        })
    }

    /// Fetch + content-address-verify the manifest pair for a
    /// `rolloutId`. The CP recomputes `sha256(manifest_bytes)` and
    /// asserts it equals `rolloutId`; mismatches are upstream
    /// corruption and the bytes are rejected before reaching the
    /// agent. The signature is fetched alongside but not verified
    /// here — the agent does that against its local trust roots.
    pub async fn fetch_pair(&self, rollout_id: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let artifact_url = self
            .artifact_url_template
            .replace(ROLLOUT_ID_PLACEHOLDER, rollout_id);
        let signature_url = self
            .signature_url_template
            .replace(ROLLOUT_ID_PLACEHOLDER, rollout_id);

        let token = signed_fetch::read_token(self.token_file.as_deref())?;
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(self.timeout)
            .build()
            .context("build rollouts-source client")?;

        let (manifest_bytes, signature_bytes) = signed_fetch::fetch_signed_pair(
            &client,
            &artifact_url,
            &signature_url,
            token.as_deref(),
        )
        .await
        .with_context(|| format!("fetch rollout pair for {rollout_id}"))?;

        // Content-address sanity: filename = rolloutId = sha256 hex
        // of the canonical manifest. Upstream serving the wrong bytes
        // for a given rolloutId is a hard fail — refuse to spread.
        let mut hasher = Sha256::new();
        hasher.update(&manifest_bytes);
        let computed = format!("{:x}", hasher.finalize());
        if computed != rollout_id {
            return Err(anyhow!(
                "rollouts source: content-address mismatch — \
                 url claimed {rollout_id} but sha256(bytes) = {computed}",
            ));
        }

        Ok((manifest_bytes, signature_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_template_without_placeholder() {
        let err = RolloutsSource::new(
            "https://example/no-placeholder.json".to_string(),
            "https://example/no-placeholder.json.sig".to_string(),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains(ROLLOUT_ID_PLACEHOLDER));
    }

    #[test]
    fn new_rejects_signature_template_without_placeholder() {
        let err = RolloutsSource::new(
            format!("https://example/{ROLLOUT_ID_PLACEHOLDER}.json"),
            "https://example/no-placeholder.json.sig".to_string(),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("signature_url_template"));
    }

    #[test]
    fn new_accepts_valid_templates() {
        let s = RolloutsSource::new(
            format!("https://example/rollouts/{ROLLOUT_ID_PLACEHOLDER}.json"),
            format!("https://example/rollouts/{ROLLOUT_ID_PLACEHOLDER}.json.sig"),
            Some(PathBuf::from("/run/agenix/token")),
        )
        .unwrap();
        assert!(s.artifact_url_template.contains(ROLLOUT_ID_PLACEHOLDER));
        assert!(s.signature_url_template.contains(ROLLOUT_ID_PLACEHOLDER));
        assert_eq!(
            s.token_file.as_deref(),
            Some(std::path::Path::new("/run/agenix/token"))
        );
    }
}
