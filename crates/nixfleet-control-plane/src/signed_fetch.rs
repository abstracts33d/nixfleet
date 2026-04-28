//! HTTP fetch primitives shared by the channel-refs and
//! revocations poll tasks.
//!
//! Both tasks need the same shape: read a Bearer token from a
//! file (optional, public sources skip), GET an artifact URL +
//! signature URL pair, return the raw bytes for the verify
//! pipeline. Centralising the helpers here avoids the two
//! parallel modules drifting on retry posture, error wording, or
//! header conventions.
//!
//! Verification stays in the per-artifact poll modules — they
//! diverge there (different artifact types, different replay
//! targets, different freshness expectations), and a generic
//! abstraction would force their failure semantics through type
//! parameters or trait objects. The fetch is the only piece
//! that's genuinely identical.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

/// Build the HTTP client used by every poll task. Single
/// configuration point for TLS posture + timeout. The 15s timeout
/// matches the prior per-module setting; faster fails the poll
/// quietly during transient upstream blips.
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("build signed-fetch client (rustls + 15s timeout)")
}

/// Read a Bearer token from `path`, trimming surrounding
/// whitespace. Read fresh on each poll so token rotation
/// propagates without a CP restart. `None` skips auth — public
/// sources don't need a token.
pub fn read_token(path: Option<&Path>) -> Result<Option<String>> {
    match path {
        Some(p) => Ok(Some(
            std::fs::read_to_string(p)
                .with_context(|| format!("read token file {}", p.display()))?
                .trim()
                .to_string(),
        )),
        None => Ok(None),
    }
}

/// Fetch the (artifact, signature) byte pair from a configured
/// upstream URL pair, with an optional Bearer token on both
/// requests. Returns the raw bytes — verification is the
/// caller's job.
///
/// Failure semantics: any non-2xx response or network error
/// surfaces as `Err` with the URL + status + body in the message.
/// The caller logs at warn and retains its previous state. This
/// matches the "log warn + retain" posture documented in
/// `channel_refs_poll` and `revocations_poll`.
pub async fn fetch_signed_pair(
    client: &reqwest::Client,
    artifact_url: &str,
    signature_url: &str,
    token: Option<&str>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let artifact = fetch_url(client, artifact_url, token).await?;
    let signature = fetch_url(client, signature_url, token).await?;
    Ok((artifact, signature))
}

async fn fetch_url(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<Vec<u8>> {
    let mut req = client.get(url);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{url}: {status}: {body}");
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("read body {url}"))?;
    Ok(bytes.to_vec())
}
