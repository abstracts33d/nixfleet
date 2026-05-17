//! On-demand HTTP-fetched rollout manifests. This module is a thin
//! signed-pair fetcher: it substitutes the canonical RolloutId
//! (`{channel}@{channel_ref}` per RFC-0012 §6.3) into the URL templates
//! and returns the raw (manifest, signature) byte pair. It performs no
//! identifier validation. The caller (manifest_poll) is responsible for
//! signature verification (`verify_rollout_manifest`) and identifier
//! discrimination (parsed `RolloutId` equality against the requested id);
//! both checks are mandated by the `verify_rollout_manifest` docstring.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::polling::signed_fetch;

pub const ROLLOUT_ID_PLACEHOLDER: &str = "{rolloutId}";

#[derive(Debug, Clone)]
pub struct RolloutsSource {
    /// Must contain `{rolloutId}`.
    pub artifact_url_template: String,
    /// Must contain `{rolloutId}`.
    pub signature_url_template: String,
    /// `None` -> unauthenticated GET.
    pub token_file: Option<PathBuf>,
    pub timeout: Duration,
}

impl RolloutsSource {
    /// Bails if either template lacks the placeholder.
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

    /// Substitutes `rollout_id` into the URL templates and returns the
    /// (manifest, signature) byte pair. Performs no identifier validation;
    /// the caller is contractually required to invoke
    /// `verify_rollout_manifest` (authenticity) and then assert that the
    /// parsed manifest's `RolloutId::new(&m.channel, &m.channel_ref)` equals
    /// the `rollout_id` passed here (identity-substitution defense).
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

        signed_fetch::fetch_signed_pair(&client, &artifact_url, &signature_url, token.as_deref())
            .await
            .with_context(|| format!("fetch rollout pair for {rollout_id}"))
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
