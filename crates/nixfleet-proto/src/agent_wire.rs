//! Agent ↔ control-plane wire types (RFC-0003 §4).
//!
//! Defined in this crate (rather than in either binary) so the agent
//! and CP serialise/deserialise from one schema and the Nix harness
//! can reuse the same types for assertions. The checkin body adds
//! `pendingGeneration`, `lastEvaluatedTarget`, `lastFetchOutcome`,
//! and `uptimeSecs` on top of RFC-0003 §4.1's minimum — all nullable,
//! additive over the wire.
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
// /v1/agent/checkin — RFC-0003 §4.1 + checkin-body expansion
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
    /// Useful for the operator to see what the agent *would*
    /// activate even if no activation has occurred yet.
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

    /// Wall-clock moment the agent posted its most recent successful
    /// `/v1/agent/confirm` for the currently-running generation.
    /// `None` for legacy agents that don't track this, for first-boot
    /// before any confirm, and for hosts that activated outside the
    /// CP's dispatch path.
    ///
    /// Gap B of `docs/roadmap/0002-v0.2-completeness-gaps.md`: the
    /// CP repopulates `host_rollout_state.last_healthy_since` from
    /// this attestation when the host is converged on its target
    /// closure but the CP has no soak-marker row (typical after a
    /// CP rebuild — soak state was wiped, agent's record survived).
    /// CP-side clamps to `min(now, last_confirmed_at)` so a clock-
    /// skewed agent can't artificially advance the soak gate.
    /// Wire-additive — old agents leave it None and the CP's
    /// behaviour is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_confirmed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRef {
    pub closure_hash: String,
    /// Channel ref this closure was published from, if known. May
    /// be null when the agent has not yet correlated its current
    /// generation to a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_ref: Option<String>,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGeneration {
    pub closure_hash: String,
    /// Wall-clock time the pending generation is scheduled to take
    /// over (typically `null` — pending means "queued for next boot,
    /// no deadline").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatedTarget {
    pub closure_hash: String,
    pub channel_ref: String,
    pub evaluated_at: DateTime<Utc>,
    /// Rollout id this target is bound to (RFC-0003 §4.1). Format
    /// `<channel>@<short-ci-commit-or-closure>` per dispatch
    /// emission. `None` only for legacy / synthetic targets that
    /// pre-date the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
    /// Index of this host in `fleet.waves[host.channel]` (0-based).
    /// `None` when the channel has no wave plan declared (the lab
    /// fleet's single-channel single-wave deploy stays `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave_index: Option<u32>,
    /// Activation policy bound to this target (RFC-0003 §4.1).
    /// `None` only for legacy synthetic targets that pre-date the
    /// field; when present, the agent honours the supplied
    /// `confirmWindowSecs` rather than its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<ActivateBlock>,
    /// `meta.signedAt` of the `fleet.resolved.json` artifact that
    /// produced this target — relayed by the CP so the agent can
    /// run a defense-in-depth freshness check (issue #13). `None`
    /// from older CPs that pre-date the field; the agent's freshness
    /// gate fails open in that case (compatibility), and surfaces a
    /// warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    /// Channel's `freshness_window` in seconds — relayed alongside
    /// `signed_at` so the agent can enforce the same staleness gate
    /// the CP enforces at tick start. `None` from older CPs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_window_secs: Option<u32>,
    /// Operator-controlled runtime compliance gate policy (issue #57).
    /// Wire-form string, one of `"disabled"`, `"permissive"`,
    /// `"enforce"`, or `"auto"`. `None` means the CP didn't set a
    /// policy and the agent should auto-detect (Permissive when the
    /// `compliance-evidence-collector.service` unit is present,
    /// Disabled when absent). Wire-additive — old CPs leave it None
    /// and agent behaviour matches the auto-detect default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compliance_mode: Option<String>,
}

/// Activation policy embedded in `EvaluatedTarget` per RFC-0003
/// §4.1. The agent receives `confirmEndpoint` + `confirmWindowSecs`
/// alongside the closure to activate so it knows where and within
/// what deadline to POST `/v1/agent/confirm` after switching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateBlock {
    /// Seconds the agent has to POST `/v1/agent/confirm` after
    /// activating before the CP triggers magic rollback. Sourced
    /// from CP-side configuration (currently a constant; may become
    /// per-channel or per-wave when wave staging lands).
    pub confirm_window_secs: u32,
    /// HTTP path the agent POSTs to with `ConfirmRequest`.
    /// Currently always `/v1/agent/confirm`. Carried on the wire
    /// so that future endpoint relocations don't need an agent
    /// rebuild + redeploy.
    pub confirm_endpoint: String,
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

/// POST /v1/agent/checkin response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResponse {
    /// The closure hash + channel-ref the CP wants this host to
    /// move to. Null when the host is converged or no dispatch is
    /// in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<EvaluatedTarget>,
    pub next_checkin_secs: u32,
}

// =====================================================================
// /v1/agent/confirm — RFC-0003 §4.2 (activation confirmation)
// =====================================================================

/// POST /v1/agent/confirm request body.
///
/// Agent posts this exactly once after a new generation has booted
/// and the agent process has come up healthy. CP records the
/// confirmation; the magic-rollback timer (separate task) checks
/// `pending_confirms.confirm_deadline` and transitions expired
/// records to `rolled-back` if no confirm arrived in the window.
///
/// Body shape per RFC-0003 §4.2 — minus probeResults (future work).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    pub hostname: String,
    /// Rollout identifier the agent is confirming. The dispatch loop
    /// assigns rollout IDs when populating CheckinResponse.target;
    /// the agent echoes back what it acted on. Format:
    /// `<channel>@<ref>` per RFC-0003 examples.
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
/// fields may be added without a major bump).
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
    /// Pre-fire signal from the agent that it has *started* a
    /// fire-and-forget activation cycle (ADR-011). Posted before
    /// `systemd-run --unit=nixfleet-switch` queues the detached
    /// switch-to-configuration unit, so operators reading the CP
    /// report log get a deterministic "host X is now mid-activation
    /// targeting closure Y" timestamp instead of inferring it from
    /// the dispatch + confirm timing window. Purely observability —
    /// the CP doesn't gate any decision on this event (dispatch
    /// already happened at checkin time, confirm/410 happens after
    /// the poll either succeeds or times out).
    ActivationStarted {
        closure_hash: String,
        channel_ref: String,
    },

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

    /// Agent refused to activate a target because nix's substituter
    /// trust check rejected the closure's narinfo signature against
    /// the keys in `nixfleet.trust.cacheKeys` (issue #12 root #2).
    /// Cache-substituter-agnostic — fires for harmonia, attic,
    /// cachix, or any other nix-cache-protocol backend. Distinct
    /// from `RealiseFailed` so the operator dashboard can route
    /// "trust violation" alerts separately from "transient fetch
    /// failure". `stderrTail` is the last few hundred bytes of
    /// `nix-store --realise` stderr — capped to keep ReportEvent
    /// payloads small.
    ClosureSignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },

    /// Agent refused to activate a target because the backing
    /// `fleet.resolved.json` is older than the channel's
    /// `freshness_window` (issue #13). Defense-in-depth — the CP
    /// applies the same gate at tick start, so seeing this event
    /// usually indicates either a clock-skew bug or the CP's gate
    /// failed open. `closureHash` and `channelRef` identify the
    /// refused target; `signedAt` and `freshnessWindowSecs` come
    /// from the CP's relay; `ageSecs` is the agent-computed
    /// `now - signedAt` at decision time.
    StaleTarget {
        closure_hash: String,
        channel_ref: String,
        signed_at: DateTime<Utc>,
        freshness_window_secs: u32,
        age_secs: i64,
    },

    /// Runtime compliance gate refused the activation: a control's
    /// post-activation probe reported `non-compliant` or `error`.
    /// Posted by the agent for every failed control found in the
    /// freshly-collected `evidence.json`. The CP rollout engine
    /// uses these events as wave-promotion gates (a host with
    /// outstanding ComplianceFailure events stays in its current
    /// wave). Issue arcanesys/nixfleet#4 / abstracts33d/nixfleet#57.
    ///
    /// `evidence_snippet` is the probe's `checks` JSON object (the
    /// same the runtime probe writes into evidence.json),
    /// truncated to ~1KB to bound report size. `frameworkArticles`
    /// lists the article references (e.g. `["nis2:21(b)",
    /// "iso27001:A.8.15"]`) so the CP / audit log carry regulatory
    /// context without a separate framework lookup.
    ComplianceFailure {
        control_id: String,
        status: String,
        framework_articles: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_snippet: Option<serde_json::Value>,
        evidence_collected_at: DateTime<Utc>,
    },

    /// Runtime compliance gate could not produce a verdict — the
    /// `compliance-evidence-collector` service failed, timed out,
    /// or wrote evidence with a timestamp older than the moment
    /// activation completed. Distinct from `ComplianceFailure`
    /// (a per-control negative result on fresh evidence): this
    /// event signals "we couldn't measure", which the CP must
    /// treat as a confirm-blocker — same severity class as
    /// `SwitchFailed`. The freshness-verify-after-async-trigger
    /// pattern mirrors ADR-011 fire-and-forget: don't trust an
    /// async trigger ran; verify the observable post-condition.
    RuntimeGateError {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collector_exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_collected_at: Option<DateTime<Utc>>,
        activation_completed_at: DateTime<Utc>,
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
