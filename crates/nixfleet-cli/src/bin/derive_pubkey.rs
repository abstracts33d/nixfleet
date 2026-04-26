//! Tiny operator-workstation helper: read raw ed25519 private key
//! bytes from a file, print base64-encoded public key. Used once
//! per fleet-life to derive what goes into
//! `nixfleet.trust.orgRootKey.current` in fleet.nix.
//!
//! This isn't shipped with the CP — it's a build-time scratch binary
//! the operator runs locally. Lives next to `mint_token.rs` so it
//! shares the workspace's ed25519-dalek + base64 deps.

use anyhow::{Context, Result};
use base64::Engine;
use ed25519_dalek::{SigningKey, VerifyingKey};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: nixfleet-derive-pubkey <private-key-path>")?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {path}"))?;

    let arr: [u8; 32] = if bytes.len() >= 32 {
        bytes[..32].try_into().unwrap()
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
