//! Bootstrap token + enrollment + renewal wire types.
//!
//! Token format: `{version, claims, signature}` where `signature` is
//! a detached ed25519 signature over the JCS canonical bytes of
//! `claims`. The org root pubkey lives in `trust.json` under
//! `orgRootKey.current`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Bootstrap token (operator-minted, signed by org root key) ────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapToken {
    /// Bumped on incompatible claim changes; consumers MUST refuse
    /// unknown versions.
    pub version: u32,
    pub claims: TokenClaims,
    /// Base64-encoded ed25519 signature over the JCS canonical bytes
    /// of `claims`.
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenClaims {
    pub hostname: String,
    /// SHA-256 fingerprint of the expected CSR public key, base64-
    /// encoded. Binds the token to a specific keypair so a leaked
    /// token can't be used with an attacker-controlled key.
    pub expected_pubkey_fingerprint: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Random 16-byte nonce, hex-encoded. Backs replay detection.
    pub nonce: String,
}

// ─── /v1/enroll ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub token: BootstrapToken,
    /// PEM-encoded CSR. CP validates CN against
    /// `token.claims.hostname` and pubkey against
    /// `token.claims.expected_pubkey_fingerprint`.
    pub csr_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub cert_pem: String,
    pub not_after: DateTime<Utc>,
}

// ─── /v1/agent/renew ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewRequest {
    /// PEM-encoded CSR. CP validates CN matches the requesting
    /// agent's verified mTLS CN, and the CSR pubkey differs from the
    /// existing cert's pubkey (key rotation is the point of /renew).
    pub csr_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewResponse {
    pub cert_pem: String,
    pub not_after: DateTime<Utc>,
}
