//! Host probe-output signing — signs JCS-canonical event payloads
//! with `/etc/ssh/ssh_host_ed25519_key`. CP verifies against
//! `fleet.nix` `hosts.<hostname>.pubkey`.
//!
//! Why SSH host key (not mTLS cert): the auditor trust root needs to
//! rotate independently from the mTLS cert. A leaked agent cert
//! doesn't compromise the third-party auditor chain.
//!
//! Best-effort: missing/unreadable/wrong-algorithm key returns
//! `None`; agent posts events unsigned and CP flags them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;

pub use nixfleet_proto::evidence_signing::{
    ActivationFailedSignedPayload, ComplianceFailureSignedPayload,
    RollbackTriggeredSignedPayload, RuntimeGateErrorSignedPayload,
};

pub const DEFAULT_SSH_HOST_KEY_PATH: &str = "/etc/ssh/ssh_host_ed25519_key";

pub struct EvidenceSigner {
    signing_key: SigningKey,
}

impl EvidenceSigner {
    /// `Ok(None)` when the file is absent (graceful); `Err` only on
    /// parse errors, wrong algorithm, or non-NotFound IO.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %path.display(),
                    "ssh host key not found — evidence signing disabled (no auditor chain)",
                );
                return Ok(None);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("read {}", path.display()));
            }
        };

        let private = ssh_key::PrivateKey::from_openssh(&raw)
            .with_context(|| format!("parse OpenSSH key at {}", path.display()))?;

        // OpenSSH stores 64 bytes (32 seed + 32 pubkey); dalek
        // wants the 32-byte seed only.
        let key_data = match private.key_data() {
            ssh_key::private::KeypairData::Ed25519(kp) => kp.private.to_bytes(),
            other => {
                anyhow::bail!(
                    "ssh host key at {} is not ed25519 (algorithm: {:?})",
                    path.display(),
                    other.algorithm()
                );
            }
        };
        let signing_key = SigningKey::from_bytes(&key_data);

        Ok(Some(Self { signing_key }))
    }

    /// Returns base64-standard 64-byte ed25519 sig. Errors only on
    /// serde failure (would indicate a buggy ReportEvent variant).
    pub fn sign<T: Serialize>(&self, payload: &T) -> Result<String> {
        let canonical = serde_jcs::to_vec(payload)
            .context("JCS canonicalisation of evidence payload failed")?;
        let sig = self.signing_key.sign(&canonical);
        Ok(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()))
    }
}

/// Hex-lowercase SHA-256 of JCS-canonical bytes. Binds
/// `evidence_snippet` to the signed envelope without inflating size.
pub fn sha256_jcs<T: Serialize>(payload: &T) -> Result<String> {
    use sha2::Digest;
    let canonical = serde_jcs::to_vec(payload).context("JCS canonicalisation failed")?;
    let digest = sha2::Sha256::digest(&canonical);
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Default key path resolver — for use in main.rs CLI wiring.
pub fn default_ssh_host_key_path() -> PathBuf {
    PathBuf::from(DEFAULT_SSH_HOST_KEY_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    fn write_test_key(dir: &Path) -> PathBuf {
        // Generate an ed25519 keypair, write OpenSSH-encoded private
        // key (no passphrase) to a temp file, return the path.
        // Avoid SigningKey::generate (gated behind rand_core feature
        // we don't pull in for the agent runtime); roll the seed by
        // hand from the rand crate the agent already depends on.
        use ed25519_dalek::SigningKey;
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let sk = SigningKey::from_bytes(&seed);
        let kp = ssh_key::private::Ed25519Keypair {
            public: ssh_key::public::Ed25519PublicKey(sk.verifying_key().to_bytes()),
            private: ssh_key::private::Ed25519PrivateKey::from_bytes(&sk.to_bytes()),
        };
        let pk = ssh_key::PrivateKey::new(
            ssh_key::private::KeypairData::Ed25519(kp),
            "test-host",
        )
        .expect("ssh PrivateKey::new");
        let pem = pk
            .to_openssh(ssh_key::LineEnding::LF)
            .expect("to_openssh");
        let path = dir.join("ssh_host_ed25519_key");
        std::fs::write(&path, pem.as_bytes()).expect("write key");
        path
    }

    #[test]
    fn load_returns_none_when_missing() {
        let result = EvidenceSigner::load(Path::new("/nonexistent/key"));
        match result {
            Ok(None) => {}
            other => panic!("expected Ok(None), got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn sign_produces_verifiable_signature() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_test_key(dir.path());
        let signer = EvidenceSigner::load(&path)
            .expect("load")
            .expect("signer present");

        let payload = ComplianceFailureSignedPayload {
            hostname: "lab",
            rollout: Some("edge-slow@abc"),
            control_id: "auditLogging",
            status: "non-compliant",
            framework_articles: &["nis2:21(b)".to_string()],
            evidence_collected_at: chrono::Utc::now(),
            evidence_snippet_sha256: "deadbeef".to_string(),
        };

        let sig_b64 = signer.sign(&payload).expect("sign");
        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(&sig_b64)
            .expect("base64 decode");
        let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().expect("64-byte sig");
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

        // Verify with the matching pubkey.
        let canonical = serde_jcs::to_vec(&payload).expect("canonicalise");
        let vk = signer.signing_key.verifying_key();
        vk.verify(&canonical, &sig).expect("verify");
    }

    #[test]
    fn sign_changes_when_payload_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_test_key(dir.path());
        let signer = EvidenceSigner::load(&path)
            .expect("load")
            .expect("signer present");

        let p1 = ComplianceFailureSignedPayload {
            hostname: "lab",
            rollout: Some("edge-slow@abc"),
            control_id: "auditLogging",
            status: "non-compliant",
            framework_articles: &[],
            evidence_collected_at: chrono::Utc::now(),
            evidence_snippet_sha256: "aaa".to_string(),
        };
        let mut p2 = p1.clone();
        p2.control_id = "backupRetention";

        let s1 = signer.sign(&p1).expect("sign 1");
        let s2 = signer.sign(&p2).expect("sign 2");
        assert_ne!(s1, s2);
    }

    #[test]
    fn sha256_jcs_is_stable() {
        let v = serde_json::json!({"a": 1, "b": [2, 3]});
        let h1 = sha256_jcs(&v).unwrap();
        let h2 = sha256_jcs(&v).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // 32 bytes hex
    }

    #[test]
    fn sha256_jcs_differs_on_field_change() {
        let v1 = serde_json::json!({"a": 1});
        let v2 = serde_json::json!({"a": 2});
        assert_ne!(sha256_jcs(&v1).unwrap(), sha256_jcs(&v2).unwrap());
    }
}
