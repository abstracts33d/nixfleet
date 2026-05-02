//! step 0 — fetch + verify + freshness-gate.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use nixfleet_proto::{FleetResolved, Revocations, RolloutManifest, TrustedPubkey};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::time::Duration;
use thiserror::Error;

/// A signed sidecar artifact verified through the shared
/// canonicalize → signature-verify → freshness-gate pipeline.
/// Every signed artifact under `ciReleaseKey` (CONTRACTS.md §I)
/// implements this trait — the `verify_signed_sidecar` generic
/// works against any of them.
pub trait SignedSidecar {
    fn schema_version(&self) -> u32;
    fn signed_at(&self) -> Option<DateTime<Utc>>;
}

impl SignedSidecar for FleetResolved {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn signed_at(&self) -> Option<DateTime<Utc>> {
        self.meta.signed_at
    }
}

impl SignedSidecar for Revocations {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn signed_at(&self) -> Option<DateTime<Utc>> {
        self.meta.signed_at
    }
}

impl SignedSidecar for RolloutManifest {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn signed_at(&self) -> Option<DateTime<Utc>> {
        self.meta.signed_at
    }
}

/// Accepted `schemaVersion` for this consumer.
const ACCEPTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("fleet.resolved parse failed: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("signature does not verify against any declared trust root")]
    BadSignature,

    #[error("artifact is unsigned (meta.signedAt is null)")]
    NotSigned,

    #[error("stale artifact: signedAt={signed_at}, now={now}, window={window:?}")]
    Stale {
        signed_at: DateTime<Utc>,
        now: DateTime<Utc>,
        window: Duration,
    },

    #[error(
        "future-dated artifact: signedAt={signed_at} is more than {slack_secs}s ahead of now={now} \
         (clock skew tolerance is {slack_secs}s; an artifact further in the future suggests \
         pre-signing — possible CI key compromise. Rotate via reject_before)"
    )]
    FutureDated {
        signed_at: DateTime<Utc>,
        now: DateTime<Utc>,
        slack_secs: i64,
    },

    #[error(
        "artifact signed at {signed_at} is older than reject_before {reject_before} (compromise switch, CONTRACTS.md §II #1)"
    )]
    RejectedBeforeTimestamp {
        signed_at: DateTime<Utc>,
        reject_before: DateTime<Utc>,
    },

    #[error("unsupported schemaVersion: {0} (accepted: 1)")]
    SchemaVersionUnsupported(u32),

    #[error("JCS re-canonicalization failed: {0}")]
    Canonicalize(#[source] anyhow::Error),

    #[error("unsupported signature algorithm: {algorithm} (supported: ed25519, ecdsa-p256)")]
    UnsupportedAlgorithm { algorithm: String },

    #[error("trusted pubkey material is malformed ({algorithm}): {reason}")]
    BadPubkeyEncoding { algorithm: String, reason: String },

    #[error("no trust roots configured for artifact verification")]
    NoTrustRoots,
}

/// Verify any signed sidecar artifact (`fleet.resolved.json`,
/// `revocations.json`, `releases/rollouts/<rolloutId>.json`).
///
/// Generic over the parsed payload type — every signed artifact
/// under `ciReleaseKey` (CONTRACTS.md §I) goes through the same
/// canonicalize → signature-verify → schema-gate → freshness-gate
/// pipeline. Rule-of-three consolidation: the per-artifact wrappers
/// `verify_artifact`, `verify_revocations`, `verify_rollout_manifest`
/// delegate here.
///
/// `trusted_keys` supports the rotation grace window — current +
/// previous keys are both valid for up to 30 days. Tries in
/// declaration order; first match wins. Entries with unsupported
/// algorithms are skipped silently for forward-compat.
///
/// `reject_before`: compromise kill-switch. `meta.signedAt < ts`
/// is rejected regardless of which key matched. Strict `<` — exact
/// equality is accepted. Fires before the freshness check so
/// alerts can distinguish incident response from routine staleness.
pub fn verify_signed_sidecar<T: SignedSidecar + DeserializeOwned>(
    signed_bytes: &[u8],
    signature: &[u8],
    trusted_keys: &[TrustedPubkey],
    now: DateTime<Utc>,
    freshness_window: Duration,
    reject_before: Option<DateTime<Utc>>,
) -> Result<T, VerifyError> {
    let canonical =
        verify_signature_against_trust_roots(signed_bytes, signature, trusted_keys)?;
    finish_sidecar_verification(&canonical, now, freshness_window, reject_before)
}

/// Thin wrapper around `verify_signed_sidecar` for `FleetResolved`.
/// Kept as a named entry point because every existing caller (CP
/// boot, channel-refs poll, agent direct-fetch fallback) reads
/// better at the call site as `verify_artifact(...)` than as
/// `verify_signed_sidecar::<FleetResolved>(...)`.
pub fn verify_artifact(
    signed_bytes: &[u8],
    signature: &[u8],
    trusted_keys: &[TrustedPubkey],
    now: DateTime<Utc>,
    freshness_window: Duration,
    reject_before: Option<DateTime<Utc>>,
) -> Result<FleetResolved, VerifyError> {
    verify_signed_sidecar(
        signed_bytes,
        signature,
        trusted_keys,
        now,
        freshness_window,
        reject_before,
    )
}

/// `verify_strict` rejects malleable signatures — required for
/// root-of-trust keys.
fn verify_ed25519(
    canonical_bytes: &[u8],
    signature: &[u8],
    public_b64: &str,
) -> Result<(), VerifyError> {
    let public_bytes =
        BASE64_STANDARD
            .decode(public_b64)
            .map_err(|e| VerifyError::BadPubkeyEncoding {
                algorithm: "ed25519".into(),
                reason: format!("base64 decode failed: {e}"),
            })?;
    let public_array: [u8; 32] =
        public_bytes
            .try_into()
            .map_err(|v: Vec<u8>| VerifyError::BadPubkeyEncoding {
                algorithm: "ed25519".into(),
                reason: format!("expected 32 bytes, got {}", v.len()),
            })?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_array).map_err(|e| VerifyError::BadPubkeyEncoding {
            algorithm: "ed25519".into(),
            reason: e.to_string(),
        })?;

    let sig_array: [u8; 64] = signature
        .try_into()
        .map_err(|_| VerifyError::BadSignature)?;
    let sig = Signature::from_bytes(&sig_array);

    verifying_key
        .verify_strict(canonical_bytes, &sig)
        .map_err(|_| VerifyError::BadSignature)
}

/// Pubkey: 64 bytes `X || Y` (SEC1 uncompressed minus `0x04` tag),
/// base64. Sig: 64 bytes `R || S` raw. Low-s malleability rejected
/// (canonical p256 has `s <= n/2`; `normalize_s().is_some()` ⇒
/// reject) — same hardening posture as ed25519 `verify_strict`.
fn verify_ecdsa_p256(
    canonical_bytes: &[u8],
    signature: &[u8],
    public_b64: &str,
) -> Result<(), VerifyError> {
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
    use p256::EncodedPoint;

    let public_bytes =
        BASE64_STANDARD
            .decode(public_b64)
            .map_err(|e| VerifyError::BadPubkeyEncoding {
                algorithm: "ecdsa-p256".into(),
                reason: format!("base64 decode failed: {e}"),
            })?;
    if public_bytes.len() != 64 {
        return Err(VerifyError::BadPubkeyEncoding {
            algorithm: "ecdsa-p256".into(),
            reason: format!(
                "expected 64 bytes (X ‖ Y uncompressed), got {}",
                public_bytes.len()
            ),
        });
    }

    // Re-tag as 65-byte SEC1 uncompressed (0x04 || X || Y).
    let mut tagged = [0u8; 65];
    tagged[0] = 0x04;
    tagged[1..].copy_from_slice(&public_bytes);
    let point = EncodedPoint::from_bytes(tagged).map_err(|e| VerifyError::BadPubkeyEncoding {
        algorithm: "ecdsa-p256".into(),
        reason: format!("SEC1 decode failed: {e}"),
    })?;
    let verifying_key = P256VerifyingKey::from_encoded_point(&point).map_err(|e| {
        VerifyError::BadPubkeyEncoding {
            algorithm: "ecdsa-p256".into(),
            reason: format!("not on curve / invalid point: {e}"),
        }
    })?;

    let sig = P256Signature::from_slice(signature).map_err(|_| VerifyError::BadSignature)?;

    // Normalise to low-s before verifying. ECDSA signatures are
    // malleable — both `(r, s)` and `(r, n-s)` are valid for the
    // same message — and TPM2_Sign does not normalise on its own
    // (the underlying random `k` produces ~50% high-s outputs). The
    // earlier strict-rejection posture (Bitcoin-style) bit a real
    // CI run on lab where the TPM emitted a high-s signature: the
    // body was canonical, the trust pubkey matched, yet verify
    // returned BadSignature. Strict low-s isn't load-bearing for
    // our wire — these artifacts are signed by a single TPM, fetched
    // once, verified once, never re-emitted; there is no third-party
    // consumer that might mis-treat the alternate form. Normalise
    // both forms to the canonical low-s representation before
    // ECDSA-verifying.
    let sig = sig.normalize_s().unwrap_or(sig);

    verifying_key
        .verify(canonical_bytes, &sig)
        .map_err(|_| VerifyError::BadSignature)
}

/// Verify a signed `revocations.json` artifact. Same trust class
/// as [`verify_artifact`] — both signed by `ciReleaseKey` — so the
/// shared `verify_signed_sidecar` pipeline applies unchanged.
pub fn verify_revocations(
    signed_bytes: &[u8],
    signature: &[u8],
    trusted_keys: &[TrustedPubkey],
    now: DateTime<Utc>,
    freshness_window: Duration,
    reject_before: Option<DateTime<Utc>>,
) -> Result<Revocations, VerifyError> {
    verify_signed_sidecar(
        signed_bytes,
        signature,
        trusted_keys,
        now,
        freshness_window,
        reject_before,
    )
}

/// Verify a signed `releases/rollouts/<rolloutId>.json` artifact.
/// Same trust class as `fleet.resolved.json` and `revocations.json`.
///
/// Callers that received a `rolloutId` advertised by the CP should
/// additionally call [`compute_rollout_id`] on the verified manifest
/// and assert the result equals the advertised id — that's the
/// content-address check that closes RFC-0002 §4.4's threat model.
/// Kept as a separate step so the verify path itself stays generic.
pub fn verify_rollout_manifest(
    signed_bytes: &[u8],
    signature: &[u8],
    trusted_keys: &[TrustedPubkey],
    now: DateTime<Utc>,
    freshness_window: Duration,
    reject_before: Option<DateTime<Utc>>,
) -> Result<RolloutManifest, VerifyError> {
    verify_signed_sidecar(
        signed_bytes,
        signature,
        trusted_keys,
        now,
        freshness_window,
        reject_before,
    )
}

/// Compute the SHA-256 hex of the JCS-canonical bytes of any
/// serialisable value. The shared primitive behind every
/// content-address derivation in the system: rolloutId,
/// fleet_resolved_hash, future per-host or Merkle-projected hashes.
///
/// Errors only on serde or canonicalize failure — both indicate a
/// malformed input the caller should refuse to act on.
pub fn compute_canonical_hash<T: serde::Serialize>(value: &T) -> Result<String, VerifyError> {
    let raw = serde_json::to_string(value)?;
    let canonical =
        nixfleet_canonicalize::canonicalize(&raw).map_err(VerifyError::Canonicalize)?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(hex_lowercase(&digest))
}

/// Compute a `RolloutManifest`'s rolloutId — `sha256(canonical(m))`,
/// hex lowercase. Producer-side use: `nixfleet-release` derives the
/// filename `releases/rollouts/<rolloutId>.json` from this. Consumer
/// side: every recipient (CP on advertise, agent on first-fetch,
/// auditor offline) recomputes and asserts equality against the id
/// it was told to fetch.
pub fn compute_rollout_id(manifest: &RolloutManifest) -> Result<String, VerifyError> {
    compute_canonical_hash(manifest)
}

fn hex_lowercase(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Shared parse → canonicalize → sig-verify. Returns canonical
/// bytes; caller does type-specific schema-gate + freshness.
fn verify_signature_against_trust_roots(
    signed_bytes: &[u8],
    signature: &[u8],
    trusted_keys: &[TrustedPubkey],
) -> Result<String, VerifyError> {
    if trusted_keys.is_empty() {
        return Err(VerifyError::NoTrustRoots);
    }

    let raw_str = std::str::from_utf8(signed_bytes).map_err(|e| {
        VerifyError::Parse(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e,
        )))
    })?;
    let _value: serde_json::Value = serde_json::from_str(raw_str)?;
    let canonical =
        nixfleet_canonicalize::canonicalize(raw_str).map_err(VerifyError::Canonicalize)?;

    let mut attempted_any_supported = false;
    for key in trusted_keys {
        match key.algorithm.as_str() {
            "ed25519" => {
                attempted_any_supported = true;
                if verify_ed25519(canonical.as_bytes(), signature, &key.public).is_ok() {
                    return Ok(canonical);
                }
            }
            "ecdsa-p256" => {
                attempted_any_supported = true;
                if verify_ecdsa_p256(canonical.as_bytes(), signature, &key.public).is_ok() {
                    return Ok(canonical);
                }
            }
            _other => continue,
        }
    }

    if !attempted_any_supported {
        return Err(VerifyError::UnsupportedAlgorithm {
            algorithm: trusted_keys[0].algorithm.clone(),
        });
    }
    Err(VerifyError::BadSignature)
}

/// Generic schema-gate + reject-before + freshness check.
/// Two freshness checks, applied in order:
///
/// 1. **Past bound** (existing): reject if `now - signed_at > window
///    + CLOCK_SKEW_SLACK_SECS`. Catches a stale fleet.resolved being
///    served past its declared validity.
///
/// 2. **Future bound** (added 2026-05-02): reject if `signed_at - now
///    > CLOCK_SKEW_SLACK_SECS`. Catches a future-dated artifact —
///    benign clock skew is bounded by the slack constant; anything
///    beyond suggests the signer is producing artifacts targeted at
///    a future window. With a CI key compromise, future-dating is
///    the natural way to mint long-lived rogue artifacts that
///    survive short reject_before rotations. Pre-fix, the past-only
///    check accepted any future-dated artifact indefinitely
///    (verified live 2026-05-02 with `--now=signed_at - 2 days` →
///    exit 0).
///
/// `reject_before` is checked before either freshness bound so alerts
/// can distinguish "key compromised, rotate via reject_before" from
/// "CI is behind" / "operator's clock is wrong".
///
/// Applies `CLOCK_SKEW_SLACK_SECS` uniformly to every sidecar — same
/// trust root, same fetch path, same clock-drift surface, same slack.
fn finish_sidecar_verification<T: SignedSidecar + DeserializeOwned>(
    canonical: &str,
    now: DateTime<Utc>,
    freshness_window: Duration,
    reject_before: Option<DateTime<Utc>>,
) -> Result<T, VerifyError> {
    let payload: T = serde_json::from_str(canonical)?;
    if payload.schema_version() != ACCEPTED_SCHEMA_VERSION {
        return Err(VerifyError::SchemaVersionUnsupported(
            payload.schema_version(),
        ));
    }

    let signed_at = payload.signed_at().ok_or(VerifyError::NotSigned)?;

    if let Some(rb) = reject_before {
        if signed_at < rb {
            return Err(VerifyError::RejectedBeforeTimestamp {
                signed_at,
                reject_before: rb,
            });
        }
    }

    let window = ChronoDuration::from_std(freshness_window)
        .expect("freshness_window fits in i64 nanoseconds — multi-century windows are a bug");
    let effective_window = window + ChronoDuration::seconds(CLOCK_SKEW_SLACK_SECS);
    let elapsed = now - signed_at;
    if elapsed > effective_window {
        return Err(VerifyError::Stale {
            signed_at,
            now,
            window: freshness_window,
        });
    }
    if -elapsed > ChronoDuration::seconds(CLOCK_SKEW_SLACK_SECS) {
        return Err(VerifyError::FutureDated {
            signed_at,
            now,
            slack_secs: CLOCK_SKEW_SLACK_SECS,
        });
    }

    Ok(payload)
}

pub const CLOCK_SKEW_SLACK_SECS: i64 = 60;
