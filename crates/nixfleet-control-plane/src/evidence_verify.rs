//! CP-side verifier for host probe-output signatures.
//!
//! Reconstructs the JCS-canonical signed payload from the wire
//! `ReportEvent` + cert-CN-bound hostname, parses
//! `hosts.<hostname>.pubkey` from `fleet.resolved.json` (OpenSSH
//! format), and runs `ed25519::verify_strict`.
//!
//! Hostname comes from the cert, not the report body — the report
//! handler already enforced `cert_cn == body.hostname`. Pubkey
//! absence (`NoPubkey`) is graceful: lab fleets enrol hosts before
//! stamping `pubkey` in fleet.nix.

use anyhow::{Context, Result};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Serialize;

pub use nixfleet_proto::evidence_signing::{
    ActivationFailedSignedPayload, ComplianceFailureSignedPayload,
    RollbackTriggeredSignedPayload, RuntimeGateErrorSignedPayload,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignatureStatus {
    Verified,
    /// Agent pre-dates the signature field, or the host has no SSH
    /// ed25519 key.
    Unsigned,
    /// Signature present but the host has no pubkey declared in
    /// fleet.resolved (lab pre-enrollment state).
    NoPubkey,
    /// verify_strict refused — active tampering, should alarm.
    Mismatch,
    /// Signature decoding or pubkey parse failed — active tampering.
    Malformed,
    /// Host's pubkey is non-ed25519 (RSA/ECDSA). Currently
    /// unsupported; soft skip.
    WrongAlgorithm,
}

impl SignatureStatus {
    /// CP gating trust comes from mTLS (report handler enforces
    /// `cert_cn == body.hostname` upstream of this verdict). The
    /// probe-output signature is defense-in-depth + auditor-chain
    /// seam, not the primary trust root. Everything counts except
    /// statuses that signal active tampering.
    ///
    /// A future "strict" channel mode requiring `Verified` for
    /// gate participation belongs as a new `GateMode` variant
    /// not a flip of this boolean.
    pub fn counts_for_gate(self) -> bool {
        !matches!(self, SignatureStatus::Mismatch | SignatureStatus::Malformed)
    }
}

/// Verify base64 ed25519 `signature` over JCS-canonical bytes of
/// `payload`. Inputs are Optional so callers pass Options through;
/// the verdict reflects each Some/None combination.
pub fn verify_event<T: Serialize>(
    signature: Option<&str>,
    pubkey_openssh: Option<&str>,
    payload: &T,
) -> SignatureStatus {
    let Some(sig_b64) = signature else {
        return SignatureStatus::Unsigned;
    };
    let Some(pubkey_str) = pubkey_openssh else {
        return SignatureStatus::NoPubkey;
    };

    let pubkey = match parse_ssh_ed25519_pubkey(pubkey_str) {
        Ok(Some(k)) => k,
        Ok(None) => return SignatureStatus::WrongAlgorithm,
        Err(_) => return SignatureStatus::Malformed,
    };

    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(sig_b64) {
        Ok(b) => b,
        Err(_) => return SignatureStatus::Malformed,
    };
    let sig_arr: [u8; 64] = match sig_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return SignatureStatus::Malformed,
    };
    let sig = Signature::from_bytes(&sig_arr);

    let canonical = match serde_jcs::to_vec(payload) {
        Ok(v) => v,
        Err(_) => return SignatureStatus::Malformed,
    };

    match pubkey.verify(&canonical, &sig) {
        Ok(()) => SignatureStatus::Verified,
        Err(_) => SignatureStatus::Mismatch,
    }
}

/// `Ok(Some(_))` for a well-formed ed25519 pubkey, `Ok(None)` for
/// non-ed25519 algorithms (caller → `WrongAlgorithm`), `Err(_)` on
/// parse failure (caller → `Malformed`).
fn parse_ssh_ed25519_pubkey(line: &str) -> Result<Option<VerifyingKey>> {
    let public = ssh_key::PublicKey::from_openssh(line.trim())
        .context("parse OpenSSH pubkey")?;
    match public.key_data() {
        ssh_key::public::KeyData::Ed25519(ed) => {
            let bytes: [u8; 32] = ed.0;
            let vk = VerifyingKey::from_bytes(&bytes)
                .context("ed25519 verifying key from 32 bytes")?;
            Ok(Some(vk))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate an ed25519 keypair (avoiding the rand_core feature
    /// gate on `SigningKey::generate`), return (signing_key, ssh
    /// public key string in `ssh-ed25519 AAAAC3...` form).
    fn fresh_keypair() -> (ed25519_dalek::SigningKey, String) {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let pubkey_bytes = sk.verifying_key().to_bytes();
        let ssh_pk = ssh_key::PublicKey::new(
            ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(pubkey_bytes)),
            "test-host",
        );
        let openssh = ssh_pk.to_openssh().expect("to_openssh");
        (sk, openssh)
    }

    fn sample_payload() -> ComplianceFailureSignedPayload<'static> {
        ComplianceFailureSignedPayload {
            hostname: "lab",
            rollout: Some("edge-slow@abc"),
            control_id: "auditLogging",
            status: "non-compliant",
            framework_articles: &[],
            evidence_collected_at: chrono::DateTime::from_timestamp(1_000_000, 0).unwrap(),
            evidence_snippet_sha256: "deadbeef".to_string(),
        }
    }

    #[test]
    fn verify_returns_unsigned_when_signature_missing() {
        let payload = sample_payload();
        assert_eq!(
            verify_event(None, Some("ssh-ed25519 AAAAxxxx"), &payload),
            SignatureStatus::Unsigned
        );
    }

    #[test]
    fn verify_returns_no_pubkey_when_pubkey_missing() {
        let payload = sample_payload();
        assert_eq!(
            verify_event(Some("AAAA"), None, &payload),
            SignatureStatus::NoPubkey
        );
    }

    #[test]
    fn verify_round_trip_succeeds() {
        use ed25519_dalek::Signer;
        let (sk, pubkey_str) = fresh_keypair();
        let payload = sample_payload();
        let canonical = serde_jcs::to_vec(&payload).unwrap();
        let sig = sk.sign(&canonical);
        let sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        assert_eq!(
            verify_event(Some(&sig_b64), Some(&pubkey_str), &payload),
            SignatureStatus::Verified
        );
    }

    #[test]
    fn verify_returns_mismatch_on_tampered_payload() {
        use ed25519_dalek::Signer;
        let (sk, pubkey_str) = fresh_keypair();
        let payload = sample_payload();
        let canonical = serde_jcs::to_vec(&payload).unwrap();
        let sig = sk.sign(&canonical);
        let sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

        // Verify against a tampered payload — different control_id.
        let mut tampered = sample_payload();
        tampered.control_id = "backupRetention";
        assert_eq!(
            verify_event(Some(&sig_b64), Some(&pubkey_str), &tampered),
            SignatureStatus::Mismatch
        );
    }

    #[test]
    fn verify_returns_mismatch_on_wrong_pubkey() {
        use ed25519_dalek::Signer;
        let (sk_signer, _) = fresh_keypair();
        let (_, pubkey_str_other) = fresh_keypair();
        let payload = sample_payload();
        let canonical = serde_jcs::to_vec(&payload).unwrap();
        let sig = sk_signer.sign(&canonical);
        let sig_b64 =
            base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        // Sign with key A, verify with key B — should mismatch.
        assert_eq!(
            verify_event(Some(&sig_b64), Some(&pubkey_str_other), &payload),
            SignatureStatus::Mismatch
        );
    }

    #[test]
    fn verify_returns_malformed_on_garbage_signature() {
        let payload = sample_payload();
        let (_, pubkey_str) = fresh_keypair();
        // Not base64.
        assert_eq!(
            verify_event(Some("!!!not-base64!!!"), Some(&pubkey_str), &payload),
            SignatureStatus::Malformed
        );
        // Wrong length (32 bytes instead of 64).
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        assert_eq!(
            verify_event(Some(&short), Some(&pubkey_str), &payload),
            SignatureStatus::Malformed
        );
    }

    #[test]
    fn verify_returns_malformed_on_garbage_pubkey() {
        let payload = sample_payload();
        let sig = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
        assert_eq!(
            verify_event(Some(&sig), Some("ssh-ed25519 garbage"), &payload),
            SignatureStatus::Malformed
        );
    }

    #[test]
    fn signature_status_gate_counting() {
        // Verified, Unsigned, NoPubkey, WrongAlgorithm — all count
        // (mTLS-bound trust). Mismatch + Malformed signal active
        // tampering and DON'T count.
        assert!(SignatureStatus::Verified.counts_for_gate());
        assert!(SignatureStatus::Unsigned.counts_for_gate());
        assert!(SignatureStatus::NoPubkey.counts_for_gate());
        assert!(SignatureStatus::WrongAlgorithm.counts_for_gate());
        assert!(!SignatureStatus::Mismatch.counts_for_gate());
        assert!(!SignatureStatus::Malformed.counts_for_gate());
    }
}
