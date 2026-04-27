//! Agent ↔ control-plane wire types (RFC-0003 §4).
//!
//! Defined in this crate (rather than in either binary) so the agent
//! and CP serialise/deserialise from one schema and Stream B can
//! reuse the same types for harness assertions. The Phase 3 expansion
//! adds `pendingGeneration`, `lastEvaluatedTarget`, `lastFetchOutcome`,
//! and `uptimeSecs` to the checkin body — all nullable, additive over
//! RFC-0003 §4.1's minimum.
//!
//! Unknown-field posture follows the crate-level convention: serde's
//! default is to ignore unknowns; consumers MUST treat additions
//! within the same major version as backwards-compatible.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Protocol major version (RFC-0003 §6). Sent by the agent in
/// `X-Nixfleet-Protocol` on every `/v1/agent/*` request; CP checks
/// and rejects mismatched majors with 426 Upgrade Required.
///
/// v1 → v2 is a breaking change. Within a major, fields may be
/// added; agents and CP MUST ignore unknown fields.
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;

/// HTTP header carrying the agent's declared protocol major
/// version. Lowercase per HTTP/2 conventions (axum normalises
/// regardless).
pub const PROTOCOL_VERSION_HEADER: &str = "x-nixfleet-protocol";

// =====================================================================
// /v1/agent/checkin — RFC-0003 §4.1 + Phase 3 expansion
// =====================================================================

/// POST /v1/agent/checkin request body. Sent by the agent every
/// `pollInterval` seconds; CP records into in-memory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinRequest {
    pub hostname: String,
    pub agent_version: String,

    /// What's running right now (`/run/current-system`).
    pub current_generation: GenerationRef,

    /// What's queued for next boot if it differs from current
    /// (`/run/booted-system` vs `/run/current-system`). Null when
    /// they match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_generation: Option<PendingGeneration>,

    /// The most recent target the agent saw from the CP. Null on
    /// first checkin or before the agent has fetched a target.
    /// Phase 3 doesn't activate, but it's useful for the operator
    /// to see what the agent *would* activate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evaluated_target: Option<EvaluatedTarget>,

    /// Outcome of the most recent target fetch + verify attempt.
    /// Null if the agent hasn't tried to fetch anything yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch_outcome: Option<FetchOutcome>,

    /// Seconds since the agent process started. Useful for spotting
    /// agents that crash-loop without showing up as down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRef {
    pub closure_hash: String,
    /// Channel ref this closure was published from, if known. Null
    /// during PR-1/PR-3 because the agent doesn't yet correlate
    /// channels (PR-4 introduces the projection that does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_ref: Option<String>,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGeneration {
    pub closure_hash: String,
    /// Wall-clock time the pending generation is scheduled to take
    /// over (typically `null` in Phase 3 — pending = "queued for
    /// next boot, no deadline").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedTarget {
    pub closure_hash: String,
    pub channel_ref: String,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchOutcome {
    pub result: FetchResult,
    /// Short error string when `result != Ok`. Null when ok.
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

/// POST /v1/agent/checkin response. Phase 3 always returns
/// `target: null` (no rollouts dispatched until Phase 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    /// The closure hash + channel-ref the CP wants this host to
    /// move to. Null in Phase 3 — Phase 4's dispatch loop populates
    /// this once activation is wired up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EvaluatedTarget>,
    pub next_checkin_secs: u32,
}

// =====================================================================
// /v1/agent/confirm — RFC-0003 §4.2 (activation confirmation)
// =====================================================================

/// POST /v1/agent/confirm request body (Phase 4).
///
/// Agent posts this exactly once after a new generation has booted
/// and the agent process has come up healthy. CP records the
/// confirmation; the magic-rollback timer (separate task) checks
/// `pending_confirms.confirm_deadline` and transitions expired
/// records to `rolled-back` if no confirm arrived in the window.
///
/// Body shape per RFC-0003 §4.2 — minus probeResults (Phase 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    pub hostname: String,
    /// Rollout identifier the agent is confirming. Phase 4's
    /// dispatch loop assigns rollout IDs when populating
    /// CheckinResponse.target; the agent echoes back what it acted
    /// on. Format: `<channel>@<ref>` per RFC-0003 examples.
    pub rollout: String,
    pub wave: u32,
    /// What the agent is now running, post-activation. Same shape
    /// as CheckinRequest.currentGeneration so the CP can
    /// cross-check that the agent activated the right closure.
    pub generation: GenerationRef,
}

/// POST /v1/agent/confirm response.
///
/// 204 No Content on acceptance — body is empty. RFC-0003 §4.2:
/// "204 on acceptance, 410 Gone if the rollout was cancelled or
/// the wave already failed (agent then triggers local rollback on
/// its own)." 410 is a status-code-only response; this struct
/// covers the rare success-with-body case (currently empty —
/// future Phase 4 PRs may add fields without a major bump).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmResponse {}

// =====================================================================
// /v1/agent/report — RFC-0003 §4.5 (event reports)
// =====================================================================

/// POST /v1/agent/report request body. Agent emits this when a
/// notable event happens out-of-band from the regular checkin
/// cadence — activation failure, realisation failure, post-switch
/// verify mismatch, enrollment / renewal failure, trust-file
/// problem.
///
/// Wire shape per RFC-0003 §4.3, with two operationally-useful
/// additions on top of the RFC's minimum:
/// - `agentVersion` for triage (CP can spot mismatched-rev agents).
/// - `occurredAt` so the operator can reason about timing without
///   relying on CP-side receipt timestamp.
///
/// `event` is a discriminator string (kebab-case, see
/// [`ReportEvent`]). `details` holds per-event structured fields.
/// `rollout` correlates the event with a `pending_confirms` row
/// (matches `dispatch::Decision::Dispatch.rollout_id`); `null` for
/// events that aren't tied to a specific rollout (enrollment,
/// trust-error, …).
///
/// The earlier shipped shape (`kind` enum + free-form `error` +
/// `context: Value`) is retired here — `kind` was a closed enum
/// that needed proto bumps for new failure modes, `context: Value`
/// was opaque to operators, and there was no rollout linkage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    pub hostname: String,
    pub agent_version: String,
    pub occurred_at: DateTime<Utc>,
    /// Rollout id this event is bound to (matches the
    /// `<channel>@<short-ci-commit>` form the dispatch loop emits).
    /// `None` for events not tied to a specific rollout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<String>,
    /// `event` discriminator + per-variant `details` body.
    /// `#[serde(flatten)]` puts both at the top level of the
    /// request body, matching RFC-0003 §4.3's example exactly.
    #[serde(flatten)]
    pub event: ReportEvent,
}

/// Typed event variants. `event` is a kebab-case discriminator on
/// the wire; `details` carries the per-event structured body. New
/// failure modes add a variant — old agents/CPs see the variant
/// they don't recognise as `Other` if the consumer is permissive,
/// or surface a deserialise error for stricter callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "details", rename_all = "kebab-case")]
pub enum ReportEvent {
    /// Activation step exited non-zero — `nix-env --set`,
    /// `switch-to-configuration`, or any subsequent boot-time
    /// activation. `phase` names the failing step; `exitCode` and
    /// `stderrTail` are best-effort diagnostics.
    ActivationFailed {
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },

    /// `nix-store --realise` failed — substituter trust mismatch,
    /// network failure, or the path simply wasn't there. The agent
    /// did not switch.
    RealiseFailed {
        closure_hash: String,
        reason: String,
    },

    /// Post-switch verify caught `/run/current-system` pointing at a
    /// closure other than the dispatched target. The agent rolled
    /// back; the CP should mark the rollout suspect.
    VerifyMismatch {
        expected: String,
        actual: String,
    },

    /// Agent invoked local rollback after a SwitchFailed /
    /// VerifyMismatch / CP-410 outcome. Informational — paired
    /// with one of the above for triage context.
    RollbackTriggered {
        reason: String,
    },

    /// First-boot enrollment (`/v1/enroll`) failed.
    EnrollmentFailed {
        reason: String,
    },

    /// Periodic cert renewal (`/v1/agent/renew`) failed.
    RenewalFailed {
        reason: String,
    },

    /// `trust.json` failed to parse or wasn't found at agent
    /// startup. Agent operates degraded until restart.
    TrustError {
        reason: String,
    },

    /// Catch-all for events that don't yet have a typed variant.
    /// Keeps the wire forward-compat without a proto bump per
    /// new failure mode. `kind` is a free-form short label, `detail`
    /// is an opaque object.
    Other {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportResponse {
    /// CP echoes the event ID it assigned (UUID-like opaque
    /// string). Useful for correlation in journals.
    pub event_id: String,
}
