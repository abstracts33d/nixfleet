//! Shared HTTP fetch + Bearer-token primitive for the poll tasks.
//! Verification stays per-task — the artifacts diverge enough that
//! a generic abstraction would force semantics through trait objects.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

/// 15s timeout — faster trips on transient upstream blips.
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("build signed-fetch client (rustls + 15s timeout)")
}

/// Read fresh on each poll so token rotation propagates without
/// restart. `None` skips auth (public sources).
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

/// Non-2xx or network error → `Err`. Caller logs warn + retains
/// previous state.
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
