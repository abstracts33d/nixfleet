//! Evidence probe runner (RFC-0010 §3.1 + §7). READ-ONLY consumer of
//! the local collector unit's signed evidence file.
//!
//! The collector (compliance-evidence-collector.service) owns its own
//! systemd timer + cadence; this runner does NOT trigger it. On each
//! tick the runner:
//! 1. Reads `evidence_path` (default `/var/lib/nixfleet-compliance/evidence.json`).
//! 2. Verifies the ed25519 signature against the host's SSH ed25519
//!    public key half (RFC-0004 §5). Signature is read from
//!    `<path>.sig` (base64 64-byte sig of the JCS canonical bytes of
//!    the payload's `controls` array).
//! 3. Filters `controls` to `framework == decl.framework` and produces
//!    per-control sub_results. Aggregate Pass iff every framework-
//!    matching control is Pass.
//!
//! Any error (file missing, parse, signature mismatch, framework
//! missing) → `Fail` (RFC-0010 §6 uniform strict mode).

use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use nixfleet_state_machine::{ProbeStatus, ProbeSubResult};
use serde::Deserialize;
use std::path::Path;

use super::{ProbeDecl, RunnerOutcome};

pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    let Some(framework) = decl.framework.as_deref() else {
        return RunnerOutcome::fail(now, "evidence probe: framework missing");
    };
    let evidence_path = Path::new(&decl.evidence_path);
    let sig_path = evidence_path.with_extension("json.sig");

    let payload_bytes = match tokio::fs::read(evidence_path).await {
        Ok(b) => b,
        Err(err) => {
            return RunnerOutcome::fail(
                now,
                format!("evidence probe: read {}: {err}", evidence_path.display()),
            );
        }
    };
    let sig_b64 = match tokio::fs::read_to_string(&sig_path).await {
        Ok(s) => s.trim().to_string(),
        Err(err) => {
            return RunnerOutcome::fail(
                now,
                format!("evidence probe: read {}: {err}", sig_path.display()),
            );
        }
    };

    // Host SSH host pubkey (RFC-0004 §5). Read from a conventional
    // path; the agent's main.rs --ssh-host-key-file points at the
    // PRIVATE half, the public half is alongside as `.pub` per
    // OpenSSH convention. The agent verifies the signature against
    // the public half here.
    let pubkey_bytes = match resolve_host_pubkey().await {
        Ok(b) => b,
        Err(reason) => return RunnerOutcome::fail(now, format!("evidence probe: {reason}")),
    };
    let vk = match VerifyingKey::from_bytes(&pubkey_bytes) {
        Ok(v) => v,
        Err(err) => {
            return RunnerOutcome::fail(now, format!("evidence probe: pubkey parse: {err}"));
        }
    };
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(&sig_b64) {
        Ok(b) => b,
        Err(err) => return RunnerOutcome::fail(now, format!("evidence probe: sig base64: {err}")),
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return RunnerOutcome::fail(
            now,
            format!("evidence probe: sig length {} != 64", sig_bytes.len()),
        );
    };
    let sig = Signature::from_bytes(&sig_arr);
    if vk.verify(&payload_bytes, &sig).is_err() {
        return RunnerOutcome::fail(now, "evidence probe: signature verify failed");
    }

    // Parse payload, filter by framework, build sub_results.
    let parsed: EvidencePayload = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(err) => return RunnerOutcome::fail(now, format!("evidence probe: parse: {err}")),
    };
    let sub_results: Vec<ProbeSubResult> = parsed
        .controls
        .into_iter()
        .filter(|c| c.framework == framework)
        .map(|c| ProbeSubResult {
            control_id: c.control_id,
            status: if c.passed {
                ProbeStatus::Pass
            } else {
                ProbeStatus::Fail
            },
            framework: c.framework,
            article: c.article,
        })
        .collect();
    if sub_results.is_empty() {
        return RunnerOutcome::fail(
            now,
            format!("evidence probe: no controls match framework '{framework}'"),
        );
    }
    let all_pass = sub_results
        .iter()
        .all(|s| matches!(s.status, ProbeStatus::Pass));
    RunnerOutcome {
        status: if all_pass {
            ProbeStatus::Pass
        } else {
            ProbeStatus::Fail
        },
        observed_at: now,
        failure_reason: if all_pass {
            None
        } else {
            Some(format!(
                "evidence probe: {}: at least one control failed",
                framework
            ))
        },
        sub_results: Some(sub_results),
    }
}

async fn resolve_host_pubkey() -> Result<[u8; 32], String> {
    // The agent's CLI accepts `--ssh-host-key-file` (defaulting to
    // `/etc/ssh/ssh_host_ed25519_key`). The matching public half lives
    // alongside as `<path>.pub`. We read the public file here so the
    // probe runner doesn't need the private key (which it has no
    // business with).
    let priv_path = std::env::var("NIXFLEET_AGENT_SSH_HOST_KEY_FILE")
        .unwrap_or_else(|_| "/etc/ssh/ssh_host_ed25519_key".to_string());
    let pub_path = format!("{priv_path}.pub");
    let raw = tokio::fs::read_to_string(&pub_path)
        .await
        .map_err(|err| format!("read host pubkey {pub_path}: {err}"))?;
    // OpenSSH public key format: "ssh-ed25519 <base64-blob> <comment>".
    let blob_b64 = raw
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed host pubkey at {pub_path}"))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .map_err(|err| format!("host pubkey base64 decode: {err}"))?;
    // The blob is a length-prefixed wire format; the last 32 bytes are
    // the raw ed25519 public key (RFC-0004 §5 / RFC 4253).
    if blob.len() < 32 {
        return Err(format!("host pubkey blob len {} < 32", blob.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&blob[blob.len() - 32..]);
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct EvidencePayload {
    #[serde(default)]
    controls: Vec<ControlEntry>,
}

#[derive(Debug, Deserialize)]
struct ControlEntry {
    #[serde(rename = "controlId")]
    control_id: String,
    framework: String,
    #[serde(default)]
    article: Option<String>,
    passed: bool,
}
