#![allow(clippy::doc_lazy_continuation)]
//! JCS canonicalization library backing the `nixfleet-canonicalize`
//! binary. Pinned to `serde_jcs` per `docs/CONTRACTS.md §III`.
//!
//! Every signer and verifier in the fleet goes through this module —
//! do not reimplement in Nix, shell, or ad-hoc Rust.

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::Digest;

/// Canonicalize an arbitrary JSON string to JCS (RFC 8785) form.
///
/// Errors on malformed JSON. The returned string is the exact byte
/// sequence every signer must feed to its signature primitive and
/// every verifier must reconstruct before verification.
pub fn canonicalize(input: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(input).context("input is not valid JSON")?;
    serde_jcs::to_string(&value).context("JCS canonicalization failed")
}

/// Hex-lowercase SHA-256 of `value`'s JCS-canonical bytes.
///
/// Both signers (which compute the digest before signing) and
/// verifiers (which re-derive it from received payloads) call this
/// helper. Using a single source of truth eliminates the failure-
/// mode drift that the audit caught — the agent's signer used to
/// surface JCS errors as `Result`, the CP's reports route silently
/// returned an empty hash. Single helper, one Result-returning
/// shape, no callers can record an empty digest by accident.
pub fn sha256_jcs_hex<T: Serialize>(value: &T) -> Result<String> {
    let canonical = serde_jcs::to_vec(value).context("JCS canonicalization failed")?;
    let digest = sha2::Sha256::digest(&canonical);
    Ok(hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_jcs_hex_string_value_is_stable() {
        // Stable across calls: same input → same hex.
        let a = sha256_jcs_hex(&"hello").unwrap();
        let b = sha256_jcs_hex(&"hello").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // 32 bytes hex = 64 chars
    }

    #[test]
    fn sha256_jcs_hex_struct_value_is_stable() {
        #[derive(Serialize)]
        struct S<'a> {
            host: &'a str,
            count: u32,
        }
        let a = sha256_jcs_hex(&S {
            host: "ohm",
            count: 7,
        })
        .unwrap();
        let b = sha256_jcs_hex(&S {
            host: "ohm",
            count: 7,
        })
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn sha256_jcs_hex_empty_string_is_distinct_from_other_input() {
        // Regression: prior CP impl silently returned empty string on
        // canonicalize failure. With sha256_jcs_hex the empty input
        // is a real hash (sha256 of `""`), distinct from any other
        // input's hash.
        let empty = sha256_jcs_hex(&"").unwrap();
        let nonempty = sha256_jcs_hex(&"x").unwrap();
        assert_ne!(empty, nonempty);
        assert_eq!(empty.len(), 64);
    }
}
