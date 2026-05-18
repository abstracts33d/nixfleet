//! Consumer-side typed view of the `evidence.json` wire format
//! produced by `compliance-evidence-collector.service`.
//!
//! LOADBEARING: this struct is **one implementation** of the canonical
//! schema; the contract is the JSON-on-disk format, not the Rust type.
//! The authoritative spec lives in the producer repo at
//! `nixfleet-compliance/docs/evidence-format.md` with a reference
//! fixture at `nixfleet-compliance/docs/evidence-format-fixture.json`.
//! Multiple language implementations (this Rust struct, the bash+jq
//! emitter in `probe-runner.sh`, the bash reader in
//! `compliance-check`, the untyped `serde_json::Value` consumer in
//! `nixfleet-compliance-verify`) all conform to the same wire spec.
//!
//! Cardinality: one entry per control with a `framework_articles`
//! map. The same control (e.g. `access-control`) typically satisfies
//! multiple articles across multiple frameworks; nesting the map
//! avoids duplicating probe details on the wire. The agent expands
//! each entry into one `ProbeSubResult` per `(framework, article)`
//! tuple for per-row accounting in `probe_failures` (RFC-0007 §7.2).
//!
//! Versioning: `schema_version: 1` is the first formally-defined
//! evidence schema. Additive fields keep this constant (consumers
//! ignore unknown fields via `serde(default)`); field removal or
//! semantic change bumps the version in lockstep with the upstream
//! spec doc. The unreleased pre-retag v0.2.0 emit shape (no
//! schemaVersion field, `control` key spelling, `status` enum) is
//! superseded by the joint v0.2 retag — no production hosts ran the
//! broken integration end-to-end.
//!
//! Drift detection: `tests::fixture_round_trip` deserialises the
//! upstream reference fixture (copied verbatim from the producer
//! repo with a SYNC header). Schema changes in the producer that
//! aren't mirrored here surface as a test failure on the next build.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Schema version emitted by current producers. Bumped on any
/// field-removal or semantic change; additive fields keep this
/// constant.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level `evidence.json` shape. The signed bytes consumed by
/// `nixfleet-compliance-verify` (and the agent's signature verify
/// step) are the JCS canonical bytes of this struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceFile {
    pub schema_version: u32,
    pub hostname: String,
    pub collected_at: DateTime<Utc>,
    pub controls: Vec<EvidenceControlEntry>,
}

/// One entry per control evaluated by the compliance collector.
/// `passed` is the control-level aggregate (every check in `details`
/// passed). `framework_articles` lists which framework articles the
/// control satisfies — used by the agent to emit one
/// `ProbeSubResult` per (framework, article) tuple downstream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceControlEntry {
    /// Capability-named control identifier (e.g. `"access-control"`,
    /// `"secure-boot"`). Matches the `controlId` parameter of
    /// `nixfleet-compliance/lib/mkTypedControl.nix`.
    pub control_id: String,

    /// Control-level aggregate: `true` iff every probe check passed.
    /// Failure on any check sets this to `false`; the agent
    /// propagates this to every framework/article tuple the control
    /// covers when emitting sub-results.
    pub passed: bool,

    /// Framework → article-IDs the control satisfies. Empty map is
    /// valid (control covers no framework articles — synthetic
    /// always-fail control, smoke probe, etc.). The agent skips
    /// entries with an empty map for per-article accounting.
    #[serde(default)]
    pub framework_articles: HashMap<String, Vec<String>>,

    /// Free-form probe output for human display (compliance-check
    /// CLI, auditor reports). NOT consumed by the gate; preserved on
    /// the wire so a single signed file reproduces the operator-
    /// facing detail without re-running probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,

    /// Typed-control schema hint (e.g. `"anssi-bp028/v1"`). Optional;
    /// set only when the producer used the typed-control pipeline
    /// (nixfleet-compliance/lib/mkTypedControl.nix). Auditor tools
    /// use this to apply schema-specific decoders to `details`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixture() -> EvidenceFile {
        let mut framework_articles = HashMap::new();
        framework_articles.insert(
            "nis2-essential".to_string(),
            vec!["art21.i".to_string()],
        );
        framework_articles.insert(
            "iso27001".to_string(),
            vec!["A.5.1".to_string()],
        );
        EvidenceFile {
            schema_version: SCHEMA_VERSION,
            hostname: "agent-01".to_string(),
            collected_at: Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap(),
            controls: vec![EvidenceControlEntry {
                control_id: "access-control".to_string(),
                passed: true,
                framework_articles,
                details: Some(serde_json::json!({
                    "password_auth_disabled": true,
                    "root_login_restricted": true,
                })),
                schema: Some("anssi-bp028/v1".to_string()),
            }],
        }
    }

    #[test]
    fn evidence_file_round_trip() {
        let f = fixture();
        let json = serde_json::to_string(&f).unwrap();
        let parsed: EvidenceFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn evidence_file_serialises_camel_case() {
        let f = fixture();
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"schemaVersion\":1"));
        assert!(json.contains("\"collectedAt\""));
        assert!(json.contains("\"controlId\":\"access-control\""));
        assert!(json.contains("\"frameworkArticles\""));
    }

    #[test]
    fn evidence_file_empty_framework_articles_legal() {
        let f = EvidenceFile {
            schema_version: SCHEMA_VERSION,
            hostname: "synthetic-host".to_string(),
            collected_at: Utc.with_ymd_and_hms(2026, 5, 18, 12, 0, 0).unwrap(),
            controls: vec![EvidenceControlEntry {
                control_id: "synthetic".to_string(),
                passed: false,
                framework_articles: HashMap::new(),
                details: None,
                schema: None,
            }],
        };
        let json = serde_json::to_string(&f).unwrap();
        let parsed: EvidenceFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn evidence_file_missing_schema_version_fails() {
        // The legacy v0.2.0-unreleased compliance emit (no schemaVersion
        // at the file level) does not deserialise. The retagged
        // nixfleet-compliance emits schemaVersion=1 in lockstep.
        let no_version = serde_json::json!({
            "hostname": "agent-01",
            "collectedAt": "2026-05-18T12:00:00Z",
            "controls": [],
        });
        let err = serde_json::from_value::<EvidenceFile>(no_version).unwrap_err();
        assert!(
            err.to_string().contains("schemaVersion"),
            "missing schemaVersion must produce a clear error; got: {err}",
        );
    }

    /// Upstream-fixture roundtrip: drift detection between
    /// `nixfleet-compliance/docs/evidence-format-fixture.json` (the
    /// authoritative reference instance) and this struct. When the
    /// producer adds or renames a field, copy the upstream fixture
    /// into `src/fixtures/evidence-format.json`; this test confirms
    /// the consumer struct still parses it cleanly. Failing here
    /// means the consumer hasn't tracked an upstream schema change.
    #[test]
    fn fixture_round_trip() {
        const FIXTURE: &str = include_str!("fixtures/evidence-format.json");
        let parsed: EvidenceFile =
            serde_json::from_str(FIXTURE).expect("upstream fixture must parse");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.hostname, "water-plant-01");
        assert_eq!(parsed.controls.len(), 3);

        // Spot-check fields the consumer struct doesn't currently
        // gate on but the wire still carries — surfaces silent drops.
        let access = parsed
            .controls
            .iter()
            .find(|c| c.control_id == "access-control")
            .expect("access-control entry");
        assert!(access.passed);
        assert!(access.framework_articles.contains_key("nis2-essential"));
        assert!(access.framework_articles.contains_key("iso27001"));
        assert!(access.framework_articles.contains_key("anssi-bp028"));
        assert!(access.details.is_some());
        assert_eq!(access.schema.as_deref(), Some("anssi-bp028/v1"));

        let synthetic = parsed
            .controls
            .iter()
            .find(|c| c.control_id == "synthetic")
            .expect("synthetic entry");
        assert!(!synthetic.passed);
        assert!(synthetic.framework_articles.is_empty());
        assert!(synthetic.schema.is_none());

        // Re-serialise + re-parse: idempotent at the struct level.
        let reserialised = serde_json::to_string(&parsed).unwrap();
        let re_parsed: EvidenceFile = serde_json::from_str(&reserialised).unwrap();
        assert_eq!(re_parsed, parsed);
    }
}
