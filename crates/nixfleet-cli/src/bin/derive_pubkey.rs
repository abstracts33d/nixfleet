//! Operator helper: ed25519 private key file → base64 public key.

use anyhow::{Context, Result};
use base64::Engine;
use ed25519_dalek::{SigningKey, VerifyingKey};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: nixfleet-derive-pubkey <private-key-path>")?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;

    let arr: [u8; 32] = if bytes.len() >= 32 {
        bytes[..32]
            .try_into()
            .expect("slice of length 32 fits [u8; 32] — len checked above")
    } else {
        anyhow::bail!("expected at least 32 bytes, got {}", bytes.len());
    };
    let sk = SigningKey::from_bytes(&arr);
    let vk: VerifyingKey = sk.verifying_key();
    println!(
        "{}",
        base64::engine::general_purpose::STANDARD.encode(vk.to_bytes())
    );
    Ok(())
}
