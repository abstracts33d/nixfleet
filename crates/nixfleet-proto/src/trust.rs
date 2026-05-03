//! Trust root declarations.
//!
//! LOADBEARING: algorithm is a property of the key, not the artifact.
//! Verifier matches `(artifact, sig) → trust root → algorithm` — artifacts
//! MUST NOT carry their own algorithm claim (an attacker could otherwise
//! downgrade by lying about which algo signed the bytes).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `algorithm` is `String` (not enum) for forward-compat with future
/// algorithms. Unknown values surface as `UnsupportedAlgorithm` at verify
/// time. Today: ed25519 — `public` is 32-byte base64 (padded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPubkey {
    pub algorithm: String,
    pub public: String,
}

/// Loaded from `/etc/nixfleet/{cp,agent}/trust.json`. Restart-only reload.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustConfig {
    pub schema_version: u32,

    pub ci_release_key: KeySlot,

    /// Forwarded opaquely to `nix.settings.trusted-public-keys`.
    #[serde(default)]
    pub cache_keys: Vec<String>,

    #[serde(default)]
    pub org_root_key: Option<KeySlot>,
}

impl TrustConfig {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;
}

/// LOADBEARING: `reject_before` is the compromise kill-switch — artifacts
/// signed before this timestamp are refused regardless of which key signed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySlot {
    #[serde(default)]
    pub current: Option<TrustedPubkey>,

    #[serde(default)]
    pub previous: Option<TrustedPubkey>,

    #[serde(default)]
    pub reject_before: Option<DateTime<Utc>>,
}

impl KeySlot {
    /// LOADBEARING: returns `[current, previous]` (newer first). Verifiers
    /// iterate first-match-wins; reordering breaks the rotation grace window.
    pub fn active_keys(&self) -> Vec<TrustedPubkey> {
        let mut keys = Vec::with_capacity(2);
        if let Some(k) = &self.current {
            keys.push(k.clone());
        }
        if let Some(k) = &self.previous {
            keys.push(k.clone());
        }
        keys
    }
}
