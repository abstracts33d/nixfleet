//! RFC-0002 §4 step 0 — fetch + verify + freshness-gate.
//!
//! Implementation follows in Phase C.

use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("implementation pending")]
    Placeholder,
}

pub fn verify_artifact(
    _signed_bytes: &[u8],
    _signature: &[u8; 64],
    _pubkey: &ed25519_dalek::VerifyingKey,
    _now: DateTime<Utc>,
    _freshness_window: Duration,
) -> Result<FleetResolved, VerifyError> {
    Err(VerifyError::Placeholder)
}
