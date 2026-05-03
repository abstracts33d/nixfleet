//! Agent ↔ control-plane wire types.
//!
//! LOADBEARING: additions within a major version MUST be backwards-compatible
//! (older consumers serde-ignore unknown fields). Bump `PROTOCOL_MAJOR_VERSION`
//! for any breaking change — the CP rejects mismatched majors with 426.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Sent in `X-Nixfleet-Protocol`; CP rejects mismatched majors with 426.
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;

pub const PROTOCOL_VERSION_HEADER: &str = "x-nixfleet-protocol";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRequest {
    pub hostname: String,
    pub agent_version: String,

    /// `/run/current-system`.
    pub current_generation: GenerationRef,

    /// `/run/booted-system` when it differs from current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_generation: Option<PendingGeneration>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evaluated_target: Option<EvaluatedTarget>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch_outcome: Option<FetchOutcome>,

    /// Surfaces crash-loops that don't show up as offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,

    /// Lets CP repopulate `last_healthy_since` after a rebuild.
    /// Clamped to `min(now, last_confirmed_at)` so clock-skewed
    /// agents can't fast-forward the soak gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRef {
    pub closure_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_ref: Option<String>,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGeneration {
    pub closure_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedTarget {
    pub closure_hash: String,
    pub channel_ref: String,
    pub evaluated_at: DateTime<Utc>,
    /// Format: `<channel>@<short-ci-commit-or-closure>`.
    pub rollout_id: String,
    /// 0-based index in `fleet.waves[host.channel]`. None for channels
    /// without a wave plan (single-host channels).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<ActivateBlock>,
    /// `meta.signedAt` of the producing fleet.resolved — relayed so
    /// the agent runs a defense-in-depth freshness check.
    pub signed_at: DateTime<Utc>,
    pub freshness_window_secs: u32,
    /// `disabled` | `permissive` | `enforce` | `auto`. None → agent
    /// auto-detects via collector-unit presence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compliance_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateBlock {
    /// Seconds before CP triggers magic rollback.
    pub confirm_window_secs: u32,
    /// Required for any target the agent will confirm. The agent refuses to
    /// confirm when no `activate` block is present (treats absence as "not a
    /// confirmable target") and otherwise POSTs strictly to this path. CP
    /// must always set it for confirm-bearing targets — the agent has no
    /// hardcoded fallback. Wire-carried so endpoint moves are CP-driven.
    pub confirm_endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchOutcome {
    pub result: FetchResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FetchResult {
    Ok,
    VerifyFailed,
    FetchFailed,
    None,
}

/// CP-driven rollback directive. Idempotent at the protocol level: the
/// agent's rollback is a no-op once it's on the prior gen, so a lost
/// `RollbackTriggered` post retries on the next checkin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackSignal {
    /// Content-addressed rolloutId of the failed rollout the CP is asking
    /// the agent to revert from.
    pub rollout: String,
    /// Provenance only; the agent rolls to its own boot-loader prior entry.
    pub target_ref: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EvaluatedTarget>,
    /// Mutually exclusive with `target` in practice; if both set, the
    /// agent runs rollback synchronously before fetching `target`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackSignal>,
    pub next_checkin_secs: u32,
}

/// Posted exactly once after a new generation has booted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    pub hostname: String,
    /// Format `<channel>@<ref>`.
    pub rollout: String,
    pub wave: u32,
    pub generation: GenerationRef,
}

/// 204 on acceptance, 410 if the rollout was cancelled / wave failed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmResponse {}

/// Out-of-band event report. `rollout = None` for events not tied to one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    pub hostname: String,
    pub agent_version: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<String>,
    #[serde(flatten)]
    pub event: ReportEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "details", rename_all = "kebab-case")]
pub enum ReportEvent {
    /// Observability-only pre-fire signal.
    ActivationStarted {
        closure_hash: String,
        channel_ref: String,
    },

    ActivationFailed {
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// `nix-store --realise` failed; agent did not switch.
    RealiseFailed {
        closure_hash: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Post-switch verify caught `/run/current-system` mismatch; rolled back.
    VerifyMismatch {
        expected: String,
        actual: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    RollbackTriggered {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    EnrollmentFailed {
        reason: String,
    },

    RenewalFailed {
        reason: String,
    },

    /// `trust.json` failed to parse or was missing at startup.
    TrustError {
        reason: String,
    },

    /// Substituter trust check rejected closure narinfo signature.
    /// Distinct from `RealiseFailed` so dashboards can route trust
    /// violations separately from transient fetch failures.
    ClosureSignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Agent refused to activate due to stale fleet.resolved. CP applies
    /// the same gate; this event indicates clock-skew or CP gate failure.
    StaleTarget {
        closure_hash: String,
        channel_ref: String,
        signed_at: DateTime<Utc>,
        freshness_window_secs: u32,
        age_secs: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Per-failing-control compliance probe negative. `evidence_snippet`
    /// is the probe's `checks` JSON, truncated to ~1KB.
    ComplianceFailure {
        control_id: String,
        status: String,
        framework_articles: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_snippet: Option<serde_json::Value>,
        evidence_collected_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Manifest fetch/parse failure; agent refuses to act on the dispatch.
    ManifestMissing {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Manifest signature didn't verify against `ciReleaseKey`.
    ManifestVerifyFailed {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Manifest verified but content-address recompute / membership
    /// / pinned-bytes check failed. Hard refuse-to-act.
    ManifestMismatch {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Runtime gate couldn't produce a verdict (collector failed, timed
    /// out, or evidence stale). Distinct from `ComplianceFailure` — CP
    /// treats this as a confirm-blocker.
    RuntimeGateError {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collector_exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_collected_at: Option<DateTime<Utc>>,
        activation_completed_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Catch-all for events that don't yet have a typed variant.
    Other {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportResponse {
    pub event_id: String,
}
