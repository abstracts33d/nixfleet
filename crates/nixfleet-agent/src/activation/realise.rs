//! `nix-store --realise` wrapper + signature-error heuristic.
//!
//! Realise is the agent's "force fetch + verify before we commit
//! to switching" step. The signature-error detection is a string
//! match against nix's stderr because nix doesn't surface a
//! distinct exit code for trust-failure; the matcher is locked in
//! by per-phrasing tests so a nix wording change breaks the test
//! rather than silently downgrading to generic RealiseFailed.

use anyhow::{anyhow, Context};
use tokio::process::Command;

/// Distinct so the agent can map signature-mismatch to a different
/// `ReportEvent` than transient fetch failures.
pub enum RealiseError {
    /// Stderr trimmed to last ~500 bytes for triage.
    SignatureMismatch { stderr_tail: String },
    /// Spawn failure, network error, missing path, non-utf8 stdout, etc.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RealiseError {
    fn from(err: anyhow::Error) -> Self {
        RealiseError::Other(err)
    }
}

/// nix has several wordings for substituter-trust failures across
/// versions. The set covers 2.18+ stable phrasings plus legacy 2.x.
/// Tested in `tests::detect_signature_error_*` so a nix wording
/// change breaks the test rather than silently downgrading to
/// generic RealiseFailed.
pub fn looks_like_signature_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "lacks a valid signature",
        "no signature is trusted",
        "is not signed by any of the keys",
        "no signatures matched",
        "signature mismatch",
        "untrusted signature",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) async fn realise(store_path: &str) -> Result<String, RealiseError> {
    let output = Command::new("nix-store")
        .arg("--realise")
        .arg(store_path)
        .output()
        .await
        .with_context(|| format!("spawn nix-store --realise {store_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if looks_like_signature_error(&stderr) {
            let tail_start = stderr.len().saturating_sub(500);
            let tail = stderr[tail_start..].to_string();
            return Err(RealiseError::SignatureMismatch { stderr_tail: tail });
        }
        return Err(anyhow!(
            "nix-store --realise {store_path} exited {:?}: {stderr}",
            output.status.code()
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| anyhow!("nix-store --realise stdout not utf-8: {e}"))?;
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow!("nix-store --realise produced no output"))?;
    Ok(line.trim().to_string())
}
