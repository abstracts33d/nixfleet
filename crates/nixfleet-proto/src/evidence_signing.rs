//! Shared signing-payload shapes for host probe-output evidence
//! (issue #12 root-3 / #59).
//!
//! Both the agent (signer) and the control plane (verifier) build
//! these structs and feed them through `serde_jcs::to_vec` to get
//! the canonical bytes that go into ed25519 sign / verify. The
//! signed envelope is the agent's compliance-event details, bound
//! to the host identity (mTLS-cert-CN-equivalent hostname) and the
//! rollout id.
//!
//! ## Why these live in proto
//!
//! Agent and CP both need the exact same byte layout. Earlier
//! revisions of this cycle had two parallel `*SignedPayload`
//! struct definitions — one per crate. Any silent drift between
//! them (a field rename, a new optional field) would break
//! signature verification across the wire without a compile error
//! on either side. Lifting the structs into the contracts crate
//! makes drift impossible.
//!
//! ## Compatibility posture
//!
//! Per CONTRACTS.md §V, the wire `ReportEvent` evolves additively
//! (new optional fields). The signed payload, by contrast, must
//! stay stable across versions: adding a field here invalidates
//! every signature an old agent has posted. New fields force a
//! signing-version bump (separate proto enum), not a silent
//! addition. Mark this seam clearly when extending.
//!
//! See also: `nixfleet-agent::evidence_signer`,
//! `nixfleet-control-plane::evidence_verify`.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Signing payload for `ReportEvent::ComplianceFailure`.
///
/// `evidence_snippet_sha256` carries the SHA-256 (hex-lowercase)
/// of the JCS-canonical bytes of the snippet rather than the
/// snippet itself — keeps the signed payload bounded even when
/// the wire snippet is truncated to ~1KB. Empty hash on `None`
/// snippet (caller picks the convention; today both sides use
/// the empty string).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceFailureSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub control_id: &'a str,
    pub status: &'a str,
    pub framework_articles: &'a [String],
    pub evidence_collected_at: DateTime<Utc>,
    pub evidence_snippet_sha256: String,
}

/// Signing payload for `ReportEvent::RuntimeGateError`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeGateErrorSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub reason: &'a str,
    pub collector_exit_code: Option<i32>,
    pub evidence_collected_at: Option<DateTime<Utc>>,
    pub activation_completed_at: DateTime<Utc>,
}

/// Signing payload for `ReportEvent::ActivationFailed`. Issue G:
/// extend the auditor chain to non-compliance failures so the
/// done-criterion #2 chain (host_key → closure_hash → git_commit)
/// is closeable for activation failures too.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationFailedSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub phase: &'a str,
    pub exit_code: Option<i32>,
    /// SHA-256 of the JCS-canonical bytes of `stderr_tail` rather
    /// than the tail itself — same rationale as
    /// `ComplianceFailureSignedPayload.evidence_snippet_sha256`.
    pub stderr_tail_sha256: String,
}

/// Signing payload for `ReportEvent::RollbackTriggered`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackTriggeredSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub reason: &'a str,
}
