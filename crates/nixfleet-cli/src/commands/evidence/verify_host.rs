//! M1.3: per-host signature verification.
//!
//! For each successfully fetched host, verify the ed25519 signature
//! on the JCS-canonical evidence bytes and cross-check the fetched
//! `evidence.host.pub` against the pubkey declared in
//! `fleet.resolved.json`. A mismatch on the cross-check signals
//! either a MitM intercept or a pubkey rotation that the operator
//! has not re-signed `fleet.resolved.json` against. Either is worth
//! the auditor's attention; both produce `PubkeyMatch::Mismatch`.
//!
//! Wrapper-is-untrusted-stitching posture (per master plan): this
//! module produces a structured outcome embedded into the per-host
//! record. The aggregator does not synthesize a new signature over
//! the wrapper; verification logic lives here and the auditor can
//! re-run the equivalent offline via `nixfleet-compliance-verify`.

use anyhow::Result;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::fetch_ssh::FetchedHost;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationOutcome {
    /// True when the operator-side fetch produced both
    /// `evidence.json.sig` and `evidence.host.pub`. False = nothing
    /// to verify (the fetch failed or the host doesn't sign yet).
    pub present: bool,
    /// True when ed25519 verification of the signature against the
    /// JCS-canonical evidence bytes passed.
    pub valid: bool,
    /// Cross-check outcome between the fetched `evidence.host.pub`
    /// and the pubkey declared in `fleet.resolved.json` for this
    /// host.
    pub pubkey_matches_declared: PubkeyMatch,
    /// Raw 32-byte ed25519 public key (base64, standard alphabet).
    /// Populated when the fetched pubkey parsed cleanly. Auditor
    /// uses this to re-verify offline without re-parsing OpenSSH
    /// format.
    pub public_key_b64: Option<String>,
    /// Raw 64-byte ed25519 signature (base64, standard alphabet) the
    /// host produced over the JCS-canonical `evidence.json` bytes.
    /// Populated whenever a sidecar signature was fetched and parsed
    /// as 64 bytes, even if subsequent verification failed - the
    /// auditor needs the original bytes to replay the verify offline.
    pub signature_b64: Option<String>,
    /// Always "ed25519" at schema v1. Kept explicit so a future
    /// algorithm transition is not silent.
    pub algorithm: &'static str,
    /// Set on any verification step that fails. Free-form, intended
    /// for operator stdout + auditor record.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PubkeyMatch {
    /// Both declared and fetched present and the raw 32-byte keys
    /// match. Default trust path.
    Match,
    /// Both present, keys differ. MitM or unrecorded rotation.
    Mismatch,
    /// `fleet.resolved.json` had `pubkey: None` for this host. No
    /// cross-check possible; verification falls back to "trust the
    /// fetched key" (still records ed25519 verify outcome).
    DeclaredAbsent,
    /// SSH fetch returned no `evidence.host.pub`. No cross-check
    /// possible and no fetched pubkey to verify against. Signature
    /// verification fails as well.
    FetchedAbsent,
}

/// Verify a single fetched host against its declared pubkey (if any).
///
/// Returns a structured outcome; never propagates internal errors as
/// `Result::Err`. Cryptographic / parse failures are recorded as
/// `valid = false` with `error = Some(...)` so the aggregator records
/// them verbatim instead of aborting the run.
pub fn verify_fetched(fh: &FetchedHost, declared_pubkey: Option<&str>) -> VerificationOutcome {
    if !fh.ok {
        return VerificationOutcome {
            present: false,
            valid: false,
            pubkey_matches_declared: PubkeyMatch::FetchedAbsent,
            public_key_b64: None,
            signature_b64: None,
            algorithm: "ed25519",
            error: Some("upstream fetch did not complete".into()),
        };
    }

    let Some(host_pub_bytes) = fh.host_pubkey.as_ref() else {
        return VerificationOutcome {
            present: false,
            valid: false,
            pubkey_matches_declared: PubkeyMatch::FetchedAbsent,
            public_key_b64: None,
            signature_b64: None,
            algorithm: "ed25519",
            error: Some("no evidence.host.pub fetched".into()),
        };
    };

    let fetched_key_bytes = match parse_openssh_ed25519_pub(host_pub_bytes) {
        Ok(k) => k,
        Err(e) => {
            return VerificationOutcome {
                present: true,
                valid: false,
                pubkey_matches_declared: PubkeyMatch::FetchedAbsent,
                public_key_b64: None,
                signature_b64: None,
                algorithm: "ed25519",
                error: Some(format!("parse fetched pubkey: {e}")),
            };
        }
    };
    let fetched_b64 = base64::engine::general_purpose::STANDARD.encode(fetched_key_bytes);

    let pubkey_matches_declared = match declared_pubkey {
        None => PubkeyMatch::DeclaredAbsent,
        Some(declared) => match parse_openssh_ed25519_pub(declared.as_bytes()) {
            Ok(declared_bytes) => {
                if declared_bytes == fetched_key_bytes {
                    PubkeyMatch::Match
                } else {
                    PubkeyMatch::Mismatch
                }
            }
            Err(_) => {
                // Declared pubkey unparseable: treat as absent for
                // cross-check purposes; the signature still verifies
                // against the fetched key. Auditor sees the absent
                // signal and the fleet.resolved producer fixes it.
                PubkeyMatch::DeclaredAbsent
            }
        },
    };

    // Signature step.
    let Some(sig_bytes) = fh.signature.as_ref() else {
        return VerificationOutcome {
            present: false,
            valid: false,
            pubkey_matches_declared,
            public_key_b64: Some(fetched_b64),
            signature_b64: None,
            algorithm: "ed25519",
            error: Some("no evidence.json.sig fetched".into()),
        };
    };
    let Some(evidence_bytes) = fh.evidence_json.as_ref() else {
        return VerificationOutcome {
            present: false,
            valid: false,
            pubkey_matches_declared,
            public_key_b64: Some(fetched_b64),
            signature_b64: None,
            algorithm: "ed25519",
            error: Some("no evidence.json fetched".into()),
        };
    };

    // Capture the raw 64-byte signature regardless of verify outcome - the
    // auditor needs it to replay verification offline, including against a
    // wrapper that recorded `valid: false`.
    let signature_b64 = parse_signature_b64(sig_bytes);

    let outcome = ed25519_verify(evidence_bytes, sig_bytes, &fetched_key_bytes);
    match outcome {
        Ok(()) => VerificationOutcome {
            present: true,
            valid: true,
            pubkey_matches_declared,
            public_key_b64: Some(fetched_b64),
            signature_b64,
            algorithm: "ed25519",
            error: None,
        },
        Err(e) => VerificationOutcome {
            present: true,
            valid: false,
            pubkey_matches_declared,
            public_key_b64: Some(fetched_b64),
            signature_b64,
            algorithm: "ed25519",
            error: Some(e.to_string()),
        },
    }
}

/// Normalise the on-disk `evidence.json.sig` content (whitespace-trimmed
/// base64) into the canonical 64-byte standard-base64 form stored in the
/// wrapper. Returns `None` if the sidecar bytes do not decode to 64
/// bytes; callers fall back to leaving `signature_b64` unset.
fn parse_signature_b64(sig_bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(sig_bytes).ok()?.trim();
    let raw = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    if raw.len() != 64 {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(raw))
}

/// Parse a single-line OpenSSH ed25519 public key (e.g.
/// `ssh-ed25519 AAAA... comment\n`) to the raw 32-byte key.
fn parse_openssh_ed25519_pub(bytes: &[u8]) -> Result<[u8; 32]> {
    let s = std::str::from_utf8(bytes)?;
    let pk = ssh_key::PublicKey::from_openssh(s.trim())?;
    match pk.key_data() {
        ssh_key::public::KeyData::Ed25519(k) => Ok(k.0),
        other => anyhow::bail!("not ed25519 (algorithm: {:?})", other.algorithm()),
    }
}

/// JCS-canonicalize the evidence bytes and verify the base64 ed25519
/// signature against the 32-byte verifying key.
fn ed25519_verify(evidence_bytes: &[u8], sig_bytes: &[u8], verifying_key: &[u8; 32]) -> Result<()> {
    let evidence_str = std::str::from_utf8(evidence_bytes)?;
    let canonical = nixfleet_canonicalize::canonicalize(evidence_str)?;
    let sig_str = std::str::from_utf8(sig_bytes)?.trim();
    let sig_raw = base64::engine::general_purpose::STANDARD.decode(sig_str)?;
    let sig_arr: [u8; 64] = sig_raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature not 64 bytes"))?;
    let signature = Signature::from_bytes(&sig_arr);
    let key = VerifyingKey::from_bytes(verifying_key)?;
    key.verify(canonical.as_bytes(), &signature)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::TryRngCore;

    fn signed_fixture(declared_matches: bool) -> (FetchedHost, Option<String>) {
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.try_fill_bytes(&mut seed).unwrap();
        let sk = SigningKey::from_bytes(&seed);
        let evidence = r#"{"schemaVersion":1,"hostname":"h1"}"#;
        let canonical = nixfleet_canonicalize::canonicalize(evidence).unwrap();
        let sig = sk.sign(canonical.as_bytes());
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

        // OpenSSH-format pubkey for fetched.
        let pk_data = ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(
            sk.verifying_key().to_bytes(),
        ));
        let pk = ssh_key::PublicKey::new(pk_data, "h1-test");
        let pub_str = pk.to_openssh().unwrap();

        let fh = FetchedHost {
            hostname: "h1".into(),
            source: "ssh",
            ok: true,
            error: None,
            evidence_json: Some(evidence.as_bytes().to_vec()),
            signature: Some(format!("{sig_b64}\n").into_bytes()),
            host_pubkey: Some(format!("{pub_str}\n").into_bytes()),
            facter_json: None,
            osquery_evidence_json: None,
        };

        let declared = if declared_matches {
            Some(pub_str.clone())
        } else {
            // Generate a *different* keypair, return its pub_str.
            let mut other = [0u8; 32];
            rand::rngs::OsRng.try_fill_bytes(&mut other).unwrap();
            let osk = SigningKey::from_bytes(&other);
            let opk_data = ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(
                osk.verifying_key().to_bytes(),
            ));
            let opk = ssh_key::PublicKey::new(opk_data, "decoy");
            Some(opk.to_openssh().unwrap())
        };

        (fh, declared)
    }

    #[test]
    fn verify_fetched_round_trip_matches_declared() {
        let (fh, declared) = signed_fixture(true);
        let v = verify_fetched(&fh, declared.as_deref());
        assert!(v.present, "{v:?}");
        assert!(v.valid, "{v:?}");
        assert_eq!(v.pubkey_matches_declared, PubkeyMatch::Match);
        assert!(v.error.is_none(), "{v:?}");
    }

    #[test]
    fn verify_fetched_tampered_evidence_invalidates_signature() {
        let (mut fh, declared) = signed_fixture(true);
        // Flip one byte of the evidence: signature no longer verifies.
        if let Some(b) = fh.evidence_json.as_mut().and_then(|v| v.first_mut()) {
            *b ^= 0xff;
        }
        let v = verify_fetched(&fh, declared.as_deref());
        assert!(v.present);
        assert!(!v.valid);
        assert!(v.error.is_some());
    }

    #[test]
    fn verify_fetched_pubkey_mismatch_with_declared() {
        let (fh, declared) = signed_fixture(false);
        let v = verify_fetched(&fh, declared.as_deref());
        // Signature itself still verifies against the FETCHED pubkey.
        assert!(v.valid);
        // But the declared pubkey is different — MitM signal.
        assert_eq!(v.pubkey_matches_declared, PubkeyMatch::Mismatch);
    }

    #[test]
    fn verify_fetched_declared_absent_does_not_error() {
        let (fh, _) = signed_fixture(true);
        let v = verify_fetched(&fh, None);
        assert!(v.valid);
        assert_eq!(v.pubkey_matches_declared, PubkeyMatch::DeclaredAbsent);
    }

    #[test]
    fn verify_fetched_upstream_fetch_failure_short_circuits() {
        let fh = FetchedHost {
            hostname: "h1".into(),
            source: "ssh",
            ok: false,
            error: Some("connection refused".into()),
            evidence_json: None,
            signature: None,
            host_pubkey: None,
            facter_json: None,
            osquery_evidence_json: None,
        };
        let v = verify_fetched(&fh, None);
        assert!(!v.present);
        assert!(!v.valid);
        assert_eq!(v.pubkey_matches_declared, PubkeyMatch::FetchedAbsent);
    }
}
