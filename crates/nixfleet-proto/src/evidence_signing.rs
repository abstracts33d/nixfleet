//! Shared signing-payload shapes for host probe-output evidence.
//!
//! Both the agent (signer) and the CP (verifier) feed these structs
//! through `serde_jcs::to_vec` to produce the canonical bytes for
//! ed25519 sign/verify. Lifted into `proto` so the two sides can't
//! drift out of sync — a field rename or new optional field would
//! break verification across the wire without a compile error if
//! they lived per-crate.
//!
//! Compatibility: the wire `ReportEvent` evolves additively, but
//! the signed payload must stay stable across versions. Adding a
//! field invalidates every signature an old agent has posted, so
//! new fields force a signing-version bump (separate proto enum).

use chrono::{DateTime, Utc};
use serde::Serialize;

/// `evidence_snippet_sha256` is the SHA-256 (hex-lowercase) of the
/// JCS bytes of the snippet, not the snippet itself — keeps the
/// signed payload bounded even when the wire snippet is truncated
/// to ~1KB.
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationFailedSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub phase: &'a str,
    pub exit_code: Option<i32>,
    /// SHA-256 of the JCS bytes of `stderr_tail` — same rationale
    /// as `ComplianceFailureSignedPayload.evidence_snippet_sha256`.
    pub stderr_tail_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackTriggeredSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub reason: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealiseFailedSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub closure_hash: &'a str,
    pub reason: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyMismatchSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub expected: &'a str,
    pub actual: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosureSignatureMismatchSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub closure_hash: &'a str,
    /// SHA-256 of the JCS bytes of `stderr_tail` — same rationale
    /// as `ActivationFailedSignedPayload.stderr_tail_sha256`.
    pub stderr_tail_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleTargetSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub closure_hash: &'a str,
    pub channel_ref: &'a str,
    pub signed_at: DateTime<Utc>,
    pub freshness_window_secs: u32,
    pub age_secs: i64,
}

/// Agent could not load + parse the rollout manifest the CP
/// advertised (RFC-0002 §4.4). Distinct from `ManifestVerifyFailed`
/// (sig failed) and `ManifestMismatch` (content-address failed).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestMissingSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub rollout_id: &'a str,
    pub reason: &'a str,
}

/// Manifest fetched but signature didn't verify against the trust
/// roots the agent already holds (same `ciReleaseKey` that signs
/// `fleet.resolved.json` and `revocations.json`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestVerifyFailedSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub rollout_id: &'a str,
    pub reason: &'a str,
}

/// Manifest signed correctly but the agent's content-bound checks
/// failed: recomputed hash doesn't match advertised `rollout_id`,
/// `(hostname, wave_index)` not in `host_set`, or a previously-cached
/// rolloutId resolves to different bytes today than yesterday.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestMismatchSignedPayload<'a> {
    pub hostname: &'a str,
    pub rollout: Option<&'a str>,
    pub rollout_id: &'a str,
    pub reason: &'a str,
}
