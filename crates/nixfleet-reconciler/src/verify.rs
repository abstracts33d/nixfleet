//! RFC-0002 §4 step 0 — fetch + verify + freshness-gate.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use nixfleet_proto::FleetResolved;
use std::time::Duration;
use thiserror::Error;

/// Accepted `schemaVersion` for this consumer.
const ACCEPTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("fleet.resolved parse failed: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("signature does not verify against the pinned CI release key")]
    BadSignature,

    #[error("stale artifact: signedAt={signed_at:?}, now={now}, window={window:?}")]
    Stale {
        signed_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        window: Duration,
    },

    #[error("unsupported schemaVersion: {0}")]
    SchemaVersionUnsupported(u32),

    #[error("JCS re-canonicalization failed: {0}")]
    Canonicalize(#[source] anyhow::Error),
}

/// Verify a signed `fleet.resolved` artifact per RFC-0002 §4 step 0.
pub fn verify_artifact(
    signed_bytes: &[u8],
    signature: &[u8; 64],
    pubkey: &VerifyingKey,
    now: DateTime<Utc>,
    freshness_window: Duration,
) -> Result<FleetResolved, VerifyError> {
    // Step 1: parse as generic JSON so we can re-canonicalize it.
    let raw_str = std::str::from_utf8(signed_bytes).map_err(|e| {
        VerifyError::Parse(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e,
        )))
    })?;
    let _value: serde_json::Value = serde_json::from_str(raw_str)?;

    // Step 2: re-canonicalize via the pinned JCS library.
    let canonical =
        nixfleet_canonicalize::canonicalize(raw_str).map_err(VerifyError::Canonicalize)?;

    // Step 3: ed25519 signature verification against canonical bytes.
    let sig = Signature::from_bytes(signature);
    pubkey
        .verify(canonical.as_bytes(), &sig)
        .map_err(|_| VerifyError::BadSignature)?;

    // Step 4: now safe to type-parse.
    let fleet: FleetResolved = serde_json::from_str(&canonical)?;

    // Step 5: schemaVersion gate.
    if fleet.schema_version != ACCEPTED_SCHEMA_VERSION {
        return Err(VerifyError::SchemaVersionUnsupported(fleet.schema_version));
    }

    // Step 6: freshness.
    let signed_at = fleet.meta.signed_at;
    let ok = match signed_at {
        Some(t) => {
            let age = now - t;
            age <= ChronoDuration::from_std(freshness_window).unwrap_or(ChronoDuration::zero())
        }
        None => false,
    };
    if !ok {
        return Err(VerifyError::Stale {
            signed_at,
            now,
            window: freshness_window,
        });
    }

    Ok(fleet)
}
