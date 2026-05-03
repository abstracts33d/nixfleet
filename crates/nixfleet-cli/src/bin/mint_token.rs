//! `nixfleet-mint-token` — operator-side bootstrap token minter.

use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_proto::enroll_wire::{BootstrapToken, TokenClaims};
use rand::RngCore;

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-mint-token",
    about = "Mint a bootstrap token for first-boot fleet enrollment."
)]
struct Args {
    /// Must match the fleet.nix entry + CSR CN at enroll time.
    #[arg(long)]
    hostname: String,

    /// base64 SHA-256 of the CSR's pubkey; binds the token to the key.
    #[arg(long)]
    csr_pubkey_fingerprint: String,

    /// Org root ed25519 private key: PKCS#8 PEM, 32 raw bytes, or hex.
    #[arg(long)]
    org_root_key: PathBuf,

    #[arg(long, default_value_t = 24)]
    validity_hours: u32,

    #[arg(long, default_value_t = 1)]
    version: u32,
}

fn read_signing_key(path: &PathBuf) -> Result<SigningKey> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read org root key {}", path.display()))?;
    // FOOTGUN: detect PEM before whitespace strip — strip would collapse BEGIN/body/END and break lines().
    if let Ok(orig) = std::str::from_utf8(&bytes) {
        if orig.trim_start().starts_with("-----BEGIN") {
            let body: String = orig
                .lines()
                .filter(|l| !l.starts_with("-----"))
                .collect::<Vec<_>>()
                .join("");
            let der = base64::engine::general_purpose::STANDARD
                .decode(&body)
                .context("base64 decode PEM body")?;
            // LOADBEARING: PKCS#8 ed25519 OCTET STRING is the last 34 bytes (0x04 0x20 + 32).
            if der.len() < 34 {
                anyhow::bail!("PEM too short for PKCS#8 ed25519");
            }
            let arr: [u8; 32] = der[der.len() - 32..]
                .try_into()
                .map_err(|_| anyhow::anyhow!("PKCS#8 tail wrong size"))?;
            return Ok(SigningKey::from_bytes(&arr));
        }
    }

    let trimmed: Vec<u8> = bytes.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    if trimmed.len() == 32 {
        let arr: [u8; 32] = trimmed[..32]
            .try_into()
            .expect("slice of length 32 fits [u8; 32] — len checked above");
        return Ok(SigningKey::from_bytes(&arr));
    }
    if let Ok(s) = std::str::from_utf8(&trimmed) {
        let s = s.trim_start_matches("0x").trim();
        if s.len() == 64 {
            let raw = hex::decode(s).context("hex decode org root key")?;
            let arr: [u8; 32] = raw[..32]
                .try_into()
                .expect("hex decode of 64 chars yields 32 bytes — fits [u8; 32]");
            return Ok(SigningKey::from_bytes(&arr));
        }
    }
    anyhow::bail!(
        "couldn't parse org root key — expected 32 raw bytes, hex, or PEM PKCS#8"
    );
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pkcs8_pem_for_seed(seed: &[u8; 32]) -> String {
        // SEQUENCE(46) { v=0; AlgId(Ed25519); OCTET-STRING(32){seed} }
        let mut der = hex::decode("302e020100300506032b657004220420").unwrap();
        der.extend_from_slice(seed);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        format!("-----BEGIN PRIVATE KEY-----\n{b64}\n-----END PRIVATE KEY-----\n")
    }

    #[test]
    fn read_signing_key_accepts_pkcs8_pem() {
        let seed = [0x42u8; 32];
        let pem = pkcs8_pem_for_seed(&seed);
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(pem.as_bytes()).unwrap();
        let key = read_signing_key(&tmp.path().to_path_buf()).expect("PEM should parse");
        assert_eq!(key.to_bytes(), seed);
    }

    #[test]
    fn read_signing_key_accepts_raw_32_bytes() {
        let seed = [0x55u8; 32];
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&seed).unwrap();
        let key = read_signing_key(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(key.to_bytes(), seed);
    }

    #[test]
    fn read_signing_key_accepts_hex() {
        let seed = [0x77u8; 32];
        let hex = hex::encode(seed);
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(hex.as_bytes()).unwrap();
        let key = read_signing_key(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(key.to_bytes(), seed);
    }
}

fn random_nonce() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let signing_key = read_signing_key(&args.org_root_key)?;

    let now = Utc::now();
    let claims = TokenClaims {
        hostname: args.hostname,
        expected_pubkey_fingerprint: args.csr_pubkey_fingerprint,
        issued_at: now,
        expires_at: now + ChronoDuration::hours(args.validity_hours as i64),
        nonce: random_nonce(),
    };

    let claims_json = serde_json::to_string(&claims).context("serialize claims")?;
    let canonical =
        nixfleet_canonicalize::canonicalize(&claims_json).context("canonicalize claims")?;
    let signature = signing_key.sign(canonical.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    let token = BootstrapToken {
        version: args.version,
        claims,
        signature: sig_b64,
    };

    let out = serde_json::to_string_pretty(&token)?;
    println!("{out}");
    eprintln!("nonce: {}", token.claims.nonce);
    Ok(())
}
