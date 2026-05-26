//! `nixfleet evidence verify` — defensive re-verify against an existing
//! fleet-evidence record.
//!
//! Three independent layers, each reported separately and contributing
//! to the exit code:
//!
//! 1. **Per-host signature replay.** For each host whose
//!    `signature.signatureBytes` is populated, JCS-recanonicalise
//!    `evidence` and re-run ed25519 verify against the embedded
//!    `publicKey`. Records produced before the signature-bytes lift
//!    have `signatureBytes = null` and are reported as `SKIPPED` (not
//!    a failure - the wrapper predates the field).
//! 2. **Summary recomputation.** Call `assemble::summarize` on the
//!    loaded `hosts` array; compare against the stored `summary`.
//!    Mismatch = the wrapper's summary block was tampered with after
//!    collect wrote it (the per-host signatures might still verify;
//!    this catches the case where the attacker leaves them alone).
//! 3. **Schema sanity.** `schemaVersion == 1`, hostnames sorted
//!    ASCII-ascending (the assembler invariant).
//!
//! Trust posture preserved: verify recomputes from inputs the wrapper
//! already carries. It does not re-fetch from hosts and does not
//! synthesise a new signature. The auditor can replay the same logic
//! offline using `nixfleet-compliance-verify` on the per-host bytes
//! the record now embeds.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::assemble::summarize;
use super::schema::{FLEET_EVIDENCE_SCHEMA_VERSION, FleetEvidenceRecord, PerHost};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Path to the fleet-evidence record to re-verify.
    #[arg(long)]
    pub record: PathBuf,
}

pub async fn run(args: Args) -> Result<()> {
    let raw = std::fs::read(&args.record)
        .with_context(|| format!("read record {}", args.record.display()))?;
    let record: FleetEvidenceRecord = serde_json::from_slice(&raw)
        .with_context(|| format!("parse fleet-evidence record at {}", args.record.display()))?;

    println!(
        "evidence verify: loaded {} host(s) from {}",
        record.hosts.len(),
        args.record.display()
    );

    let per_host_outcomes: Vec<PerHostReplay> =
        record.hosts.iter().map(replay_per_host).collect();

    for outcome in &per_host_outcomes {
        println!(
            "  {:<31} sig={:<14} summary={:<8} {}",
            outcome.hostname,
            outcome.sig_label(),
            "ok",
            outcome.detail.as_deref().unwrap_or("-"),
        );
    }

    let recomputed = summarize(&record.hosts);
    let summary_ok = recomputed == record.summary;

    let schema_ok = record.schema_version == FLEET_EVIDENCE_SCHEMA_VERSION;
    let hosts_sorted_ok = record
        .hosts
        .windows(2)
        .all(|w| w[0].hostname <= w[1].hostname);

    let valid = per_host_outcomes
        .iter()
        .filter(|o| matches!(o.kind, PerHostKind::Valid))
        .count();
    let invalid = per_host_outcomes
        .iter()
        .filter(|o| matches!(o.kind, PerHostKind::Invalid))
        .count();
    let skipped = per_host_outcomes
        .iter()
        .filter(|o| matches!(o.kind, PerHostKind::Skipped))
        .count();

    println!(
        "evidence verify: {}/{} signatures re-verified, {} invalid, {} skipped; \
         summary recompute {}; schema {}",
        valid,
        record.hosts.len(),
        invalid,
        skipped,
        if summary_ok { "MATCH" } else { "MISMATCH" },
        if schema_ok && hosts_sorted_ok {
            "ok"
        } else {
            "INVARIANT-VIOLATION"
        },
    );

    if invalid > 0 {
        bail!(
            "{invalid} per-host signature(s) failed re-verification \
             (the wrapper's `valid` claim is contradicted by the embedded bytes)",
        );
    }
    if !summary_ok {
        bail!(
            "summary block does not match recomputation from `hosts` array \
             (wrapper was tampered with after collect signed nothing)",
        );
    }
    if !schema_ok {
        bail!(
            "unsupported schemaVersion {}: only {} is recognised at this verify build",
            record.schema_version,
            FLEET_EVIDENCE_SCHEMA_VERSION,
        );
    }
    if !hosts_sorted_ok {
        bail!(
            "hosts array not ASCII-ascending by hostname; assembler invariant violated, \
             expected wrapper produced by `nixfleet evidence collect`",
        );
    }
    Ok(())
}

enum PerHostKind {
    Valid,
    Invalid,
    Skipped,
}

struct PerHostReplay {
    hostname: String,
    kind: PerHostKind,
    detail: Option<String>,
}

impl PerHostReplay {
    fn sig_label(&self) -> &'static str {
        match self.kind {
            PerHostKind::Valid => "valid",
            PerHostKind::Invalid => "INVALID",
            PerHostKind::Skipped => "SKIPPED",
        }
    }
}

fn replay_per_host(host: &PerHost) -> PerHostReplay {
    // Skipped: wrapper recorded the fetch never reached the
    // verification step (e.g., SSH refused). Nothing to replay - the
    // collect-time fetch failure is the operator's signal, not the
    // auditor's.
    if !host.signature.present {
        return PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Skipped,
            detail: Some("fetch did not produce a signature at collect time".into()),
        };
    }

    let Some(sig_b64) = host.signature.signature_bytes.as_ref() else {
        return PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Skipped,
            detail: Some(
                "no signatureBytes in record (pre-lift wrapper; collect with newer CLI to replay)"
                    .into(),
            ),
        };
    };

    let Some(pubkey_b64) = host.signature.public_key.as_ref() else {
        return PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Invalid,
            detail: Some("signatureBytes present but publicKey missing".into()),
        };
    };

    let Some(evidence) = host.evidence.as_ref() else {
        return PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Invalid,
            detail: Some(
                "signatureBytes present but evidence absent or unparseable at collect time".into(),
            ),
        };
    };

    match replay_signature(evidence, sig_b64, pubkey_b64) {
        Ok(()) => PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Valid,
            detail: None,
        },
        Err(e) => PerHostReplay {
            hostname: host.hostname.clone(),
            kind: PerHostKind::Invalid,
            detail: Some(e.to_string()),
        },
    }
}

/// Replay the ed25519 verify the host originally performed: serialise
/// the parsed evidence value to JCS-canonical bytes, decode the
/// signature + pubkey, run verify.
fn replay_signature(
    evidence: &serde_json::Value,
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<()> {
    let canonical = serde_jcs::to_vec(evidence).context("JCS-canonicalise embedded evidence")?;
    let sig_raw = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .context("decode signatureBytes from base64")?;
    let sig_arr: [u8; 64] = sig_raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("signatureBytes did not decode to 64 bytes"))?;
    let pk_raw = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .context("decode publicKey from base64")?;
    let pk_arr: [u8; 32] = pk_raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("publicKey did not decode to 32 bytes"))?;
    let signature = Signature::from_bytes(&sig_arr);
    let key = VerifyingKey::from_bytes(&pk_arr).context("publicKey not a valid ed25519 point")?;
    key.verify(&canonical, &signature)
        .context("ed25519 verify failed against embedded evidence + publicKey")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::evidence::assemble::{PerHostInput, assemble};
    use crate::commands::evidence::collector::CollectorEntry;
    use crate::commands::evidence::fetch_ssh::FetchedHost;
    use crate::commands::evidence::verify_host::{PubkeyMatch, VerificationOutcome};
    use chrono::{DateTime, Utc};
    use ed25519_dalek::{Signer, SigningKey};
    use nixfleet_proto::fleet_resolved::{FleetResolved, Meta};
    use rand::TryRngCore;
    use std::collections::HashMap;

    fn empty_fleet() -> FleetResolved {
        FleetResolved {
            schema_version: 1,
            hosts: HashMap::new(),
            channels: HashMap::new(),
            rollout_policies: HashMap::new(),
            waves: HashMap::new(),
            edges: Vec::new(),
            channel_edges: Vec::new(),
            disruption_budgets: Vec::new(),
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: None,
            },
        }
    }

    fn now_fixed() -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-05-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Build a per-host input with a real ed25519 signature over the
    /// host's evidence bytes. Mirrors what verify_host::verify_fetched
    /// captures at collect time.
    fn signed_input(hostname: &str, evidence: serde_json::Value) -> PerHostInput {
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.try_fill_bytes(&mut seed).unwrap();
        let sk = SigningKey::from_bytes(&seed);
        let evidence_bytes = serde_json::to_vec(&evidence).unwrap();
        let canonical = serde_jcs::to_vec(&evidence).unwrap();
        let sig = sk.sign(&canonical);
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        let pk_b64 = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        PerHostInput {
            fetched: FetchedHost {
                hostname: hostname.into(),
                source: "ssh",
                ok: true,
                error: None,
                evidence_json: Some(evidence_bytes),
                signature: Some(sig_b64.clone().into_bytes()),
                host_pubkey: None,
                facter_json: None,
                osquery_evidence_json: None,
            },
            verification: VerificationOutcome {
                present: true,
                valid: true,
                pubkey_matches_declared: PubkeyMatch::Match,
                public_key_b64: Some(pk_b64),
                signature_b64: Some(sig_b64),
                algorithm: "ed25519",
                error: None,
            },
            collectors: vec![CollectorEntry {
                collector_id: "nix-derivation".into(),
                data: serde_json::json!({"channel": "stable"}),
            }],
        }
    }

    fn write_record(record: &FleetEvidenceRecord) -> tempfile::NamedTempFile {
        let bytes = serde_jcs::to_vec(record).unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &bytes).unwrap();
        tmp
    }

    #[tokio::test]
    async fn verify_passes_on_freshly_collected_record() {
        let fleet = empty_fleet();
        let inputs = vec![
            signed_input("a", serde_json::json!({"controls":[{"controlId":"X","passed":true,"frameworkArticles":["NIS2-21(d)"]}]})),
            signed_input("b", serde_json::json!({"controls":[{"controlId":"Y","passed":false,"frameworkArticles":[]}]})),
        ];
        let record = assemble(&fleet, None, None, inputs, now_fixed());
        let tmp = write_record(&record);
        run(Args {
            record: tmp.path().to_path_buf(),
        })
        .await
        .expect("freshly assembled record must re-verify cleanly");
    }

    #[tokio::test]
    async fn verify_catches_tampered_summary() {
        let fleet = empty_fleet();
        let inputs = vec![signed_input("a", serde_json::json!({"controls":[]}))];
        let mut record = assemble(&fleet, None, None, inputs, now_fixed());
        // Attacker bumps the passed-controls count without touching
        // the signed evidence. Per-host sigs still verify; summary
        // recomputation catches it.
        record.summary.controls_by_status.passed += 99;
        let tmp = write_record(&record);
        let err = run(Args {
            record: tmp.path().to_path_buf(),
        })
        .await
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("summary block"),
            "expected summary-mismatch error, got: {err:#}"
        );
    }

    #[tokio::test]
    async fn verify_catches_tampered_evidence_with_stale_signature() {
        let fleet = empty_fleet();
        let inputs = vec![signed_input(
            "a",
            serde_json::json!({"controls":[{"controlId":"X","passed":true,"frameworkArticles":[]}]}),
        )];
        let mut record = assemble(&fleet, None, None, inputs, now_fixed());
        // Attacker flips a control's passed flag while leaving the
        // original (now stale) signature bytes intact. Replay fails:
        // the JCS-canonical bytes of the mutated evidence no longer
        // match what the host signed.
        if let Some(serde_json::Value::Array(controls)) = record.hosts[0]
            .evidence
            .as_mut()
            .and_then(|v| v.get_mut("controls"))
            && let Some(serde_json::Value::Object(c)) = controls.first_mut()
        {
            c.insert("passed".into(), serde_json::json!(false));
        }
        // Recompute summary so the only failure is per-host sig replay.
        record.summary = summarize(&record.hosts);
        let tmp = write_record(&record);
        let err = run(Args {
            record: tmp.path().to_path_buf(),
        })
        .await
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("failed re-verification"),
            "expected per-host sig failure, got: {err:#}"
        );
    }

    #[tokio::test]
    async fn verify_skips_pre_lift_record_without_failing() {
        let fleet = empty_fleet();
        let mut record = assemble(
            &fleet,
            None,
            None,
            vec![signed_input("a", serde_json::json!({"controls":[]}))],
            now_fixed(),
        );
        // Simulate a wrapper produced before signature-bytes lift:
        // signature.signatureBytes = None.
        record.hosts[0].signature.signature_bytes = None;
        let tmp = write_record(&record);
        run(Args {
            record: tmp.path().to_path_buf(),
        })
        .await
        .expect("pre-lift record must skip per-host replay, not fail");
    }

    #[tokio::test]
    async fn verify_catches_invariant_violation_unsorted_hosts() {
        let fleet = empty_fleet();
        let mut record = assemble(
            &fleet,
            None,
            None,
            vec![
                signed_input("alpha", serde_json::json!({"controls":[]})),
                signed_input("zeta", serde_json::json!({"controls":[]})),
            ],
            now_fixed(),
        );
        // Reverse host order: invariant says ASCII-ascending; the
        // assembler always sorts, so this should never happen in
        // practice, but verify catches it.
        record.hosts.reverse();
        let tmp = write_record(&record);
        let err = run(Args {
            record: tmp.path().to_path_buf(),
        })
        .await
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("ASCII-ascending"),
            "expected sort-invariant error, got: {err:#}"
        );
    }
}
