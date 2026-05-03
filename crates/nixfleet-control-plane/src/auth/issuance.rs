//! Cert issuance for `/v1/enroll` and `/v1/agent/renew`.

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use nixfleet_proto::enroll_wire::{BootstrapToken, TokenClaims};
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose, KeyPair,
};
use sha2::{Digest, Sha256};

/// 30 days; agents self-pace renewal at 50% via `/v1/agent/renew`.
pub const AGENT_CERT_VALIDITY: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, Clone)]
pub enum AuditContext {
    Enroll { token_nonce: String },
    Renew { previous_cert_serial: String },
}

/// Cryptographic signature only; caller handles replay/hostname/fingerprint/expiry.
pub fn verify_token_signature(token: &BootstrapToken, org_root_pubkey: &[u8]) -> Result<()> {
    if token.version != 1 {
        anyhow::bail!("unsupported token version: {}", token.version);
    }
    let pubkey = VerifyingKey::from_bytes(
        org_root_pubkey
            .try_into()
            .context("orgRootKey is not 32 bytes")?,
    )
    .context("parse orgRootKey")?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&token.signature)
        .context("decode token signature base64")?;
    let signature = Signature::from_slice(&sig_bytes).context("parse ed25519 signature")?;

    // JCS canonical bytes; matches what the operator-side mint tool signed.
    let claims_json = serde_json::to_string(&token.claims).context("serialize claims")?;
    let canonical =
        nixfleet_canonicalize::canonicalize(&claims_json).context("canonicalize claims")?;
    pubkey
        .verify(canonical.as_bytes(), &signature)
        .context("verify token signature")?;
    Ok(())
}

/// Validates expiry, hostname-vs-CN, and pubkey fingerprint; caller verifies signature/replay.
pub fn validate_token_claims(
    claims: &TokenClaims,
    csr_cn: &str,
    csr_pubkey_fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    if now < claims.issued_at {
        anyhow::bail!("token issued in the future");
    }
    if now >= claims.expires_at {
        anyhow::bail!("token expired");
    }
    if csr_cn != claims.hostname {
        anyhow::bail!(
            "CSR CN ({csr_cn}) does not match token hostname ({})",
            claims.hostname
        );
    }
    if csr_pubkey_fingerprint != claims.expected_pubkey_fingerprint {
        anyhow::bail!("CSR pubkey fingerprint does not match token expected_pubkey_fingerprint");
    }
    Ok(())
}

/// Base64(SHA-256(bytes)).
pub fn fingerprint(pubkey_bytes: &[u8]) -> String {
    let digest = Sha256::digest(pubkey_bytes);
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// Issues an agent cert (clientAuth EKU + SAN dNSName=CN); caller pre-validates CN.
pub fn issue_cert(
    csr_pem: &str,
    ca_cert_path: &Path,
    ca_key_path: &Path,
    validity: Duration,
    now: DateTime<Utc>,
) -> Result<(String, DateTime<Utc>)> {
    let ca_cert_pem = std::fs::read_to_string(ca_cert_path)
        .with_context(|| format!("read fleet CA cert {}", ca_cert_path.display()))?;
    let ca_key_pem = std::fs::read_to_string(ca_key_path)
        .with_context(|| format!("read fleet CA key {}", ca_key_path.display()))?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).context("parse fleet CA key PEM")?;
    let ca_params =
        CertificateParams::from_ca_cert_pem(&ca_cert_pem).context("parse fleet CA cert PEM")?;
    let ca = ca_params
        .self_signed(&ca_key)
        .context("rebuild fleet CA from PEM (rcgen quirk)")?;

    let csr_params = CertificateSigningRequestParams::from_pem(csr_pem).context("parse CSR PEM")?;
    let cn = csr_params
        .params
        .distinguished_name
        .iter()
        .find_map(|(t, v): (&DnType, &rcgen::DnValue)| {
            if matches!(t, DnType::CommonName) {
                Some(v.clone())
            } else {
                None
            }
        })
        .context("CSR has no CN")?;

    let mut params = csr_params.params;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    // FOOTGUN: rustls/webpki rejects CN-only certs — SAN dNSName=CN is required for mTLS to work.
    let cn_str = match &cn {
        rcgen::DnValue::PrintableString(s) => s.to_string(),
        rcgen::DnValue::Utf8String(s) => s.to_string(),
        _ => format!("{:?}", cn),
    };
    params.subject_alt_names = vec![rcgen::SanType::DnsName(
        cn_str
            .clone()
            .try_into()
            .context("CN is not a valid dNSName")?,
    )];

    let not_before_sys = SystemTime::UNIX_EPOCH + Duration::from_secs(now.timestamp() as u64);
    let not_after_sys = not_before_sys + validity;
    params.not_before = not_before_sys.into();
    params.not_after = not_after_sys.into();

    let cert = params
        .signed_by(&csr_params.public_key, &ca, &ca_key)
        .context("sign cert with fleet CA")?;

    let not_after = chrono::DateTime::<Utc>::from(not_after_sys);
    Ok((cert.pem(), not_after))
}

/// Best-effort append; write failure warns but doesn't fail issuance.
pub fn audit_log(
    path: &Path,
    now: DateTime<Utc>,
    requester_cn: &str,
    issued_cn: &str,
    not_after: DateTime<Utc>,
    context: &AuditContext,
) {
    let context_str = match context {
        AuditContext::Enroll { token_nonce } => format!("enroll/nonce:{token_nonce}"),
        AuditContext::Renew {
            previous_cert_serial,
        } => format!("renew/prev:{previous_cert_serial}"),
    };
    let record = serde_json::json!({
        "at": now.to_rfc3339(),
        "requester_cn": requester_cn,
        "issued_cn": issued_cn,
        "not_after": not_after.to_rfc3339(),
        "context": context_str,
    });
    let line = serde_json::to_string(&record)
        .expect("serde_json::to_string on a json!() Value is infallible");
    if let Err(err) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "{line}")
        })
    {
        tracing::warn!(error = %err, path = %path.display(), "failed to append audit log");
    }
}
