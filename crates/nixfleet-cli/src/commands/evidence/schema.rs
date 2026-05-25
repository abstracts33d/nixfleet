//! Fleet-evidence record schema. Operator-side typed wire format.
//!
//! Schema v1. Bumped independently of the per-host evidence.json
//! schema (which lives in nixfleet-compliance at its own v1). Adding
//! optional fields is additive; removing or renaming requires a
//! bump.
//!
//! Re-running assembly on the same inputs yields byte-identical
//! serialization once M1.5 wires JCS canonicalization on the
//! output. M1.4 produces serde-default JSON; reproducibility
//! follows in M1.5.

use serde::{Deserialize, Serialize};

pub const FLEET_EVIDENCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FleetEvidenceRecord {
    pub schema_version: u32,
    pub fleet: FleetMeta,
    /// Hosts in ASCII-ascending hostname order. Determinism anchor:
    /// re-runs over the same inputs produce the same order.
    pub hosts: Vec<PerHost>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FleetMeta {
    /// Optional fleet identifier the operator passes via flag.
    /// `null` when not provided.
    #[serde(default)]
    pub name: Option<String>,
    /// Operator-side timestamp at the start of the collect run.
    pub collected_at: chrono::DateTime<chrono::Utc>,
    /// Optional human-readable operator identity (e.g., the CN of
    /// the operator's mTLS cert). `null` when not provided.
    #[serde(default)]
    pub operator_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerHost {
    pub hostname: String,
    pub fetch: FetchInfo,
    /// Verbatim parsed `evidence.json` from the host. `null` when
    /// not fetched or when the bytes did not parse as JSON.
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
    pub signature: SignatureInfo,
    /// Outputs of every `EvidenceCollector` adapter the run config
    /// invoked. Order matches the operator-side configured collector
    /// list.
    pub collectors: Vec<super::collector::CollectorEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FetchInfo {
    /// `"ssh"`, `"agent-relay"` (future), or `"local"` (test fixture).
    pub source: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInfo {
    /// Whether sidecar `evidence.json.sig` was fetched.
    pub present: bool,
    /// Whether ed25519 verification of the signature against the
    /// JCS-canonical evidence bytes passed.
    pub valid: bool,
    /// Raw 32-byte ed25519 public key (base64, standard alphabet).
    /// `null` when no fetched pubkey parsed cleanly.
    #[serde(default)]
    pub public_key: Option<String>,
    /// Always `"ed25519"` at v1. Explicit so a future algorithm
    /// transition is not silent.
    pub algorithm: String,
    /// MitM cross-check between fetched and declared pubkey.
    pub pubkey_matches_declared: super::verify_host::PubkeyMatch,
    pub verified_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub hosts_total: u32,
    pub hosts_by_signature_status: HostsBySignatureStatus,
    pub hosts_by_pubkey_match: HostsByPubkeyMatch,
    pub controls_by_status: ControlsByStatus,
    pub framework_coverage: FrameworkCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct HostsBySignatureStatus {
    pub valid: u32,
    pub invalid: u32,
    pub missing: u32,
    pub unverifiable: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub struct HostsByPubkeyMatch {
    #[serde(rename = "match")]
    pub match_count: u32,
    pub mismatch: u32,
    pub declared_absent: u32,
    pub fetched_absent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ControlsByStatus {
    pub passed: u32,
    pub failed: u32,
    pub unknown: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FrameworkCoverage {
    #[serde(rename = "NIS2")]
    pub nis2: FrameworkCounts,
    #[serde(rename = "DORA")]
    pub dora: FrameworkCounts,
    #[serde(rename = "ISO27001")]
    pub iso27001: FrameworkCounts,
    #[serde(rename = "ANSSI-BP-028")]
    pub anssi_bp028: FrameworkCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FrameworkCounts {
    pub controls_tracked: u32,
    pub controls_passed: u32,
}
