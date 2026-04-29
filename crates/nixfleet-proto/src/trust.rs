//! Trust root declarations. Per CONTRACTS.md §II, the algorithm is
//! a property of the key, not of the artifact — the verifier matches
//! `(artifact, signature) → trust root → algorithm`. Artifacts MUST
//! NOT carry their own algorithm claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `algorithm` is a `String` rather than an enum so an old proto
/// parsing a newer Nix-declared `{"algorithm": "p256", ...}` doesn't
/// crash. Unknown algorithms surface as `VerifyError::UnsupportedAlgorithm`
/// at verify time.
///
/// Today: `ed25519` only — `public` is the 32-byte Edwards-curve
/// public key, base64 (standard alphabet, padded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedPubkey {
    pub algorithm: String,
    pub public: String,
}

/// Loaded from `/etc/nixfleet/{cp,agent}/trust.json`. Materialised
/// by the NixOS scope modules from `config.nixfleet.trust`.
/// Reload model: restart-only.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustConfig {
    /// Bumped only on breaking changes; binaries refuse to start on
    /// unknown versions. Distinct from the wire-protocol schema for
    /// `fleet.resolved` (see `fleet_resolved::Meta`).
    pub schema_version: u32,

    pub ci_release_key: KeySlot,

    /// Raw strings nix accepts in `nix.settings.trusted-public-keys`
    /// — forwarded opaquely. Covers harmonia, attic, cachix, etc.
    /// interchangeably.
    #[serde(default)]
    pub cache_keys: Vec<String>,

    #[serde(default)]
    pub org_root_key: Option<KeySlot>,
}

impl TrustConfig {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;
}

/// Trust-root slot with current/previous rotation grace.
///
/// `reject_before` is the compromise switch — artifacts whose
/// `signedAt` is older than this are refused regardless of which
/// key signed them. Enforcement lives in `verify_artifact`.
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
    /// Returns `[current, previous]` (newer first) — load-bearing
    /// for the rotation semantics: first-match callers see the
    /// newer key.
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
