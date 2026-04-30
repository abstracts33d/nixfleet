//! Agent ↔ control-plane wire types. Defined in this crate so both
//! sides serialise from one schema. Unknown-field policy: serde
//! ignores them; consumers MUST treat additions within a major as
//! backwards-compatible.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Sent in `X-Nixfleet-Protocol`; CP rejects mismatched majors with
/// 426 Upgrade Required.
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;

pub const PROTOCOL_VERSION_HEADER: &str = "x-nixfleet-protocol";

// ─── /v1/agent/checkin ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRequest {
    pub hostname: String,
    pub agent_version: String,

    /// `/run/current-system`.
    pub current_generation: GenerationRef,

    /// `/run/booted-system` when it differs from current. None when
    /// they match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_generation: Option<PendingGeneration>,

    /// Most recent target the agent saw. None on first checkin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evaluated_target: Option<EvaluatedTarget>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch_outcome: Option<FetchOutcome>,

    /// Agent process uptime — surfaces crash-loops that don't show
    /// up as offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,

    /// Agent's most recent successful confirm. Lets the CP repopulate
    /// `host_rollout_state.last_healthy_since` after a CP rebuild
    /// (clamped to `min(now, last_confirmed_at)` so a clock-skewed
    /// agent can't fast-forward the soak gate).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedTarget {
    pub closure_hash: String,
    pub channel_ref: String,
    pub evaluated_at: DateTime<Utc>,
    /// Format: `<channel>@<short-ci-commit-or-closure>`. None for
    /// legacy/synthetic targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
    /// 0-based index in `fleet.waves[host.channel]`. None when the
    /// channel has no wave plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<ActivateBlock>,
    /// `meta.signedAt` of the producing fleet.resolved.json — relayed
    /// so the agent runs a defense-in-depth freshness check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_window_secs: Option<u32>,
    /// One of `"disabled"`, `"permissive"`, `"enforce"`, `"auto"`.
    /// None → agent auto-detects (Permissive when the
    /// compliance-evidence-collector unit is present, Disabled
    /// when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compliance_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateBlock {
    /// Seconds the agent has to confirm before the CP triggers
    /// magic rollback.
    pub confirm_window_secs: u32,
    /// Currently always `/v1/agent/confirm`. Carried on the wire so
    /// future endpoint moves don't require an agent rebuild.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    /// None when the host is converged or no dispatch is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EvaluatedTarget>,
    pub next_checkin_secs: u32,
}

// ─── /v1/agent/confirm ────────────────────────────────────────────────

/// Posted exactly once after a new generation has booted. The CP's
/// magic-rollback timer transitions expired pending records to
/// `rolled-back` if no confirm arrived in the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    pub hostname: String,
    /// Format `<channel>@<ref>`.
    pub rollout: String,
    pub wave: u32,
    pub generation: GenerationRef,
}

/// 204 on acceptance, 410 if the rollout was cancelled or the wave
/// already failed (agent rolls back). 410 is status-only; this struct
/// covers the rare success-with-body case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmResponse {}

// ─── /v1/agent/report ─────────────────────────────────────────────────

/// Out-of-band event report (activation failure, verify mismatch,
/// trust error, etc.). `rollout` is None for events not tied to a
/// specific rollout (enrollment, trust-error, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    pub hostname: String,
    pub agent_version: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<String>,
    /// `flatten` so `event` discriminator + per-variant `details`
    /// body sit at the top level of the request body.
    #[serde(flatten)]
    pub event: ReportEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "details", rename_all = "kebab-case")]
pub enum ReportEvent {
    /// Pre-fire signal: the agent has *started* a fire-and-forget
    /// activation cycle. Observability only.
    ActivationStarted {
        closure_hash: String,
        channel_ref: String,
    },

    /// Activation step exited non-zero (`nix-env --set`,
    /// `switch-to-configuration`, or any boot-time activation).
    /// `signature` is base64 ed25519 over the JCS-canonical
    /// `ActivationFailedSignedPayload`. Same trust model as
    /// `ComplianceFailure.signature`.
    ActivationFailed {
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// `nix-store --realise` failed (substituter trust mismatch,
    /// network failure, missing path). The agent did not switch.
    RealiseFailed {
        closure_hash: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Post-switch verify caught `/run/current-system` pointing
    /// elsewhere. The agent rolled back.
    VerifyMismatch {
        expected: String,
        actual: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Agent invoked local rollback. Paired with one of the failure
    /// events above for triage context.
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

    /// nix's substituter trust check rejected the closure's narinfo
    /// signature against keys in `nixfleet.trust.cacheKeys`. Distinct
    /// from `RealiseFailed` so dashboards can route trust violations
    /// separately from transient fetch failures. `stderr_tail` is the
    /// last few hundred bytes of stderr — capped to bound payload size.
    ClosureSignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Agent refused to activate because the backing
    /// `fleet.resolved.json` is older than the channel's
    /// `freshness_window`. The CP applies the same gate at tick
    /// start; this event indicates clock-skew or CP gate failure.
    StaleTarget {
        closure_hash: String,
        channel_ref: String,
        signed_at: DateTime<Utc>,
        freshness_window_secs: u32,
        age_secs: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// A control's post-activation probe reported `non-compliant`
    /// or `error`. Posted per failing control. The CP uses these
    /// events as wave-promotion gates.
    ///
    /// `evidence_snippet` is the probe's `checks` JSON, truncated
    /// to ~1KB. `signature` is base64 ed25519 over the JCS-canonical
    /// `ComplianceFailureSignedPayload`, signed with the host's
    /// `/etc/ssh/ssh_host_ed25519_key`. CP verifies against
    /// `hosts.<hostname>.pubkey`. None for hosts without an SSH host
    /// key — accepted but flagged unverified.
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

    /// Agent fetched `GET /v1/rollouts/<rolloutId>` and the CP
    /// returned 404, the file pair was unreadable, or the bytes
    /// didn't parse as a `RolloutManifest`. Agent refuses to act on
    /// the dispatch (RFC-0002 §4.4 / RFC-0003 §4.1).
    ManifestMissing {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Manifest bytes were fetched but signature verification failed
    /// against the trust roots the agent already holds (`ciReleaseKey`).
    /// Hard refuse-to-act — same trust class as a tampered
    /// `fleet.resolved.json`.
    ManifestVerifyFailed {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Manifest verified but content-address recompute failed (the
    /// CP-advertised `rollout_id` doesn't match `sha256(canonical(m))`),
    /// or `(hostname, wave_index)` is not in `manifest.host_set`, or
    /// a previously-cached rolloutId now resolves to different bytes.
    /// The partition attack RFC-0002 §4.4 closes — hard refuse-to-act.
    ManifestMismatch {
        rollout_id: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// Runtime gate couldn't produce a verdict — collector failed,
    /// timed out, or evidence was older than activation completion.
    /// Distinct from `ComplianceFailure` (per-control negative on
    /// fresh evidence): this is "we couldn't measure", which the CP
    /// must treat as a confirm-blocker.
    RuntimeGateError {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collector_exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_collected_at: Option<DateTime<Utc>>,
        activation_completed_at: DateTime<Utc>,
        /// Base64 ed25519 over the JCS-canonical
        /// `RuntimeGateErrorSignedPayload`. Same trust model as
        /// `ComplianceFailure.signature`.
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
