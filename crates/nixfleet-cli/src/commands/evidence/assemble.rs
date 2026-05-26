//! M1.4: stitch fetch outcomes + verifications + collector entries
//! into a canonical fleet-evidence record.
//!
//! Pure function (no I/O, no clock reads). The caller passes
//! `Utc::now()` as `now` so the assembly is reproducible against a
//! fixed clock in tests. The aggregator signs nothing: this is
//! stitching only.

use chrono::{DateTime, Utc};
use nixfleet_proto::fleet_resolved::FleetResolved;

use super::collector::CollectorEntry;
use super::fetch_ssh::FetchedHost;
use super::schema::{
    ControlsByStatus, FLEET_EVIDENCE_SCHEMA_VERSION, FetchInfo, FleetEvidenceRecord, FleetMeta,
    FrameworkCounts, FrameworkCoverage, HostsByPubkeyMatch, HostsBySignatureStatus, PerHost,
    SignatureInfo, Summary,
};
use super::verify_host::{PubkeyMatch, VerificationOutcome};

/// One per-host bundle the assembler accepts. The collect-loop in
/// `collect.rs` produces these; M1.6 fixtures synthesize them.
pub struct PerHostInput {
    pub fetched: FetchedHost,
    pub verification: VerificationOutcome,
    pub collectors: Vec<CollectorEntry>,
}

/// Assemble the canonical record. Pure: same inputs → same output
/// (once M1.5 wires JCS canonicalization on serialization).
pub fn assemble(
    _fleet: &FleetResolved,
    fleet_name: Option<String>,
    operator_identity: Option<String>,
    inputs: Vec<PerHostInput>,
    now: DateTime<Utc>,
) -> FleetEvidenceRecord {
    let mut per_hosts: Vec<PerHost> = inputs
        .into_iter()
        .map(|input| build_per_host(input, now))
        .collect();
    per_hosts.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    let summary = summarize(&per_hosts);

    FleetEvidenceRecord {
        schema_version: FLEET_EVIDENCE_SCHEMA_VERSION,
        fleet: FleetMeta {
            name: fleet_name,
            collected_at: now,
            operator_identity,
        },
        hosts: per_hosts,
        summary,
    }
}

fn build_per_host(input: PerHostInput, now: DateTime<Utc>) -> PerHost {
    let PerHostInput {
        fetched,
        verification,
        collectors,
    } = input;

    // Parse evidence bytes as JSON. Unparseable bytes leave `evidence: null`
    // and the parse error surfaces in `signature.error` if not already
    // populated.
    let (evidence, parse_error) = match fetched.evidence_json.as_ref() {
        Some(bytes) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(format!("evidence.json parse: {e}"))),
        },
        None => (None, None),
    };

    let signature_error = verification.error.clone().or(parse_error);

    PerHost {
        hostname: fetched.hostname,
        fetch: FetchInfo {
            source: fetched.source.to_string(),
            fetched_at: now,
            ok: fetched.ok,
            error: fetched.error,
        },
        evidence,
        signature: SignatureInfo {
            present: verification.present,
            valid: verification.valid,
            public_key: verification.public_key_b64,
            signature_bytes: verification.signature_b64,
            algorithm: verification.algorithm.to_string(),
            pubkey_matches_declared: verification.pubkey_matches_declared,
            verified_at: now,
            error: signature_error,
        },
        collectors,
    }
}

/// Compute the canonical `Summary` block from the per-host array.
/// Exposed so `nixfleet evidence verify` can recompute and compare
/// against the stored summary — the auditor's primary check that the
/// wrapper's summary block was not post-hoc tampered with.
pub fn summarize(hosts: &[PerHost]) -> Summary {
    let mut hosts_by_sig = HostsBySignatureStatus::default();
    let mut hosts_by_pub = HostsByPubkeyMatch::default();
    let mut controls = ControlsByStatus::default();
    let mut fw = FrameworkCoverage::default();

    for host in hosts {
        // Signature status
        if !host.fetch.ok {
            hosts_by_sig.unverifiable += 1;
        } else if host.signature.present && host.signature.valid {
            hosts_by_sig.valid += 1;
        } else if host.signature.present && !host.signature.valid {
            hosts_by_sig.invalid += 1;
        } else {
            hosts_by_sig.missing += 1;
        }

        // Pubkey match
        match host.signature.pubkey_matches_declared {
            PubkeyMatch::Match => hosts_by_pub.match_count += 1,
            PubkeyMatch::Mismatch => hosts_by_pub.mismatch += 1,
            PubkeyMatch::DeclaredAbsent => hosts_by_pub.declared_absent += 1,
            PubkeyMatch::FetchedAbsent => hosts_by_pub.fetched_absent += 1,
        }

        // Controls
        if let Some(evidence) = host.evidence.as_ref()
            && let Some(arr) = evidence.get("controls").and_then(|v| v.as_array())
        {
            for control in arr {
                tally_control(control, &mut controls, &mut fw);
            }
        }
    }

    Summary {
        hosts_total: hosts.len() as u32,
        hosts_by_signature_status: hosts_by_sig,
        hosts_by_pubkey_match: hosts_by_pub,
        controls_by_status: controls,
        framework_coverage: fw,
    }
}

fn tally_control(
    control: &serde_json::Value,
    controls: &mut ControlsByStatus,
    fw: &mut FrameworkCoverage,
) {
    let passed = control.get("passed").and_then(|v| v.as_bool());
    match passed {
        Some(true) => controls.passed += 1,
        Some(false) => controls.failed += 1,
        None => controls.unknown += 1,
    }
    if let Some(articles) = control.get("frameworkArticles").and_then(|v| v.as_array()) {
        for article in articles.iter().filter_map(|v| v.as_str()) {
            let counts = framework_bucket(article, fw);
            if let Some(c) = counts {
                c.controls_tracked += 1;
                if passed == Some(true) {
                    c.controls_passed += 1;
                }
            }
        }
    }
}

/// Article ID → framework bucket. Prefix-before-dash routing
/// (e.g. `NIS2-21(d)` → `NIS2`, `ANSSI-BP-028-R8` → `ANSSI-BP-028`).
/// Unknown prefixes return `None` and the article is ignored.
fn framework_bucket<'a>(
    article: &str,
    fw: &'a mut FrameworkCoverage,
) -> Option<&'a mut FrameworkCounts> {
    if article.starts_with("NIS2-") {
        Some(&mut fw.nis2)
    } else if article.starts_with("DORA-") {
        Some(&mut fw.dora)
    } else if article.starts_with("ISO27001-") {
        Some(&mut fw.iso27001)
    } else if article.starts_with("ANSSI-BP-028-") {
        Some(&mut fw.anssi_bp028)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::evidence::collector::CollectorEntry;
    use crate::commands::evidence::verify_host::{PubkeyMatch, VerificationOutcome};
    use nixfleet_proto::fleet_resolved::{FleetResolved, Meta};
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
        chrono::DateTime::parse_from_rfc3339("2026-05-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn input_ok(hostname: &str, evidence: serde_json::Value, sig_valid: bool) -> PerHostInput {
        let bytes = serde_json::to_vec(&evidence).unwrap();
        PerHostInput {
            fetched: FetchedHost {
                hostname: hostname.into(),
                source: "ssh",
                ok: true,
                error: None,
                evidence_json: Some(bytes),
                signature: Some(b"sig\n".to_vec()),
                host_pubkey: Some(b"pub\n".to_vec()),
                facter_json: None,
            },
            verification: VerificationOutcome {
                present: true,
                valid: sig_valid,
                pubkey_matches_declared: PubkeyMatch::Match,
                public_key_b64: Some("AAAA".into()),
                signature_b64: Some("c2lnLWZpeHR1cmU=".into()),
                algorithm: "ed25519",
                error: None,
            },
            collectors: vec![CollectorEntry {
                collector_id: "nix-derivation".into(),
                data: serde_json::json!({"channel":"stable"}),
            }],
        }
    }

    #[test]
    fn assemble_produces_deterministic_summary_from_two_hosts() {
        let fleet = empty_fleet();
        let inputs = vec![
            input_ok(
                "a",
                serde_json::json!({
                    "controls": [
                        {"controlId":"BH-01","passed":true,"frameworkArticles":["NIS2-21(d)"]},
                        {"controlId":"AL-02","passed":false,"frameworkArticles":["ANSSI-BP-028-R8"]}
                    ]
                }),
                true,
            ),
            input_ok(
                "b",
                serde_json::json!({
                    "controls": [
                        {"controlId":"BH-01","passed":true,"frameworkArticles":["NIS2-21(d)","DORA-Ch3"]}
                    ]
                }),
                true,
            ),
        ];
        let record = assemble(&fleet, None, None, inputs, now_fixed());
        assert_eq!(record.summary.hosts_total, 2);
        assert_eq!(record.summary.hosts_by_signature_status.valid, 2);
        assert_eq!(record.summary.hosts_by_pubkey_match.match_count, 2);
        assert_eq!(record.summary.controls_by_status.passed, 2);
        assert_eq!(record.summary.controls_by_status.failed, 1);
        assert_eq!(record.summary.framework_coverage.nis2.controls_tracked, 2);
        assert_eq!(record.summary.framework_coverage.nis2.controls_passed, 2);
        assert_eq!(record.summary.framework_coverage.dora.controls_tracked, 1);
        assert_eq!(record.summary.framework_coverage.dora.controls_passed, 1);
        assert_eq!(
            record
                .summary
                .framework_coverage
                .anssi_bp028
                .controls_tracked,
            1
        );
        assert_eq!(
            record
                .summary
                .framework_coverage
                .anssi_bp028
                .controls_passed,
            0
        );
    }

    #[test]
    fn assemble_orders_hosts_ascii_ascending() {
        let fleet = empty_fleet();
        let inputs = vec![
            input_ok("zeta", serde_json::json!({"controls":[]}), true),
            input_ok("alpha", serde_json::json!({"controls":[]}), true),
            input_ok("mu", serde_json::json!({"controls":[]}), true),
        ];
        let record = assemble(&fleet, None, None, inputs, now_fixed());
        let names: Vec<&str> = record.hosts.iter().map(|h| h.hostname.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn assemble_records_unverifiable_when_fetch_failed() {
        let fleet = empty_fleet();
        let inputs = vec![PerHostInput {
            fetched: FetchedHost {
                hostname: "h1".into(),
                source: "ssh",
                ok: false,
                error: Some("connection refused".into()),
                evidence_json: None,
                signature: None,
                host_pubkey: None,
                facter_json: None,
            },
            verification: VerificationOutcome {
                present: false,
                valid: false,
                pubkey_matches_declared: PubkeyMatch::FetchedAbsent,
                public_key_b64: None,
                signature_b64: None,
                algorithm: "ed25519",
                error: Some("upstream fetch did not complete".into()),
            },
            collectors: vec![],
        }];
        let record = assemble(&fleet, None, None, inputs, now_fixed());
        assert_eq!(record.summary.hosts_by_signature_status.unverifiable, 1);
        assert_eq!(record.summary.hosts_by_pubkey_match.fetched_absent, 1);
    }

    #[test]
    fn assemble_serializes_evidence_as_null_when_bytes_unparseable() {
        let fleet = empty_fleet();
        let mut input = input_ok("h1", serde_json::json!({"controls":[]}), true);
        input.fetched.evidence_json = Some(b"not json".to_vec());
        let record = assemble(&fleet, None, None, vec![input], now_fixed());
        assert!(record.hosts[0].evidence.is_none());
        // The parse-error surfaces in the signature.error slot
        // because no other verification error occurred.
        assert!(
            record.hosts[0]
                .signature
                .error
                .as_deref()
                .unwrap_or("")
                .contains("parse")
        );
    }

    #[test]
    fn assemble_then_jcs_is_byte_reproducible() {
        let fleet = empty_fleet();
        let inputs1 = vec![
            input_ok("a", serde_json::json!({"controls":[]}), true),
            input_ok("b", serde_json::json!({"controls":[]}), true),
        ];
        let inputs2 = vec![
            // Same content, different insertion order. Output bytes
            // must still match (JCS sorts keys + ASCII-ascending host
            // sort + canonical summary computation).
            input_ok("b", serde_json::json!({"controls":[]}), true),
            input_ok("a", serde_json::json!({"controls":[]}), true),
        ];
        let now = now_fixed();
        let r1 = assemble(&fleet, None, None, inputs1, now);
        let r2 = assemble(&fleet, None, None, inputs2, now);
        let b1 = serde_jcs::to_vec(&r1).unwrap();
        let b2 = serde_jcs::to_vec(&r2).unwrap();
        assert_eq!(b1, b2, "JCS bytes must be identical for same inputs");
    }

    #[test]
    fn summary_framework_coverage_groups_by_prefix() {
        let fleet = empty_fleet();
        let inputs = vec![input_ok(
            "h1",
            serde_json::json!({
                "controls": [
                    {"passed":true,"frameworkArticles":["NIS2-21(a)","ISO27001-A.8"]},
                    {"passed":false,"frameworkArticles":["UNKNOWN-PREFIX"]}
                ]
            }),
            true,
        )];
        let record = assemble(&fleet, None, None, inputs, now_fixed());
        assert_eq!(record.summary.framework_coverage.nis2.controls_tracked, 1);
        assert_eq!(
            record.summary.framework_coverage.iso27001.controls_tracked,
            1
        );
        // UNKNOWN-PREFIX should not increment any bucket.
        assert_eq!(record.summary.framework_coverage.dora.controls_tracked, 0);
        assert_eq!(
            record
                .summary
                .framework_coverage
                .anssi_bp028
                .controls_tracked,
            0
        );
    }
}
