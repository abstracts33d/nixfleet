//! Reducer inputs. Maps RFC-0008 §4.2 wire events onto reducer transitions.
//!
//! `Local*` variants are synthesized by the agent runtime from its own
//! workers (probe outputs, activation outcomes, sustained-failure timing).
//! `Remote*` variants are synthesized by the CP runtime from inbound POSTs
//! to `/v1/agent/events`. Both halves drive the same transitions: a
//! `LocalActivationCompleted` on the agent and a `RemoteActivationCompleted`
//! on the CP mirror both move `Activating → Soaking` with the same
//! invariants applied (RFC-0009 §2 principle 4).

use chrono::{DateTime, Utc};
use nixfleet_proto::OnHealthFailure;

use crate::state::{ClosureHash, ProbeMode, ProbeName, ProbeStatus, ProbeSubResult};

/// One entry in a `LocalProbeTopologyDeclared` / `RemoteProbeTopologyDeclared`
/// event. Carries the per-probe metadata CP needs to evaluate the gate
/// without reading the agent's filesystem (RFC-0010 §8). Threading
/// `mode` per-event would also work, but the upfront declaration also
/// lets the gate distinguish "this enforce probe declared but never
/// reported" from "no enforce probe declared at all" — the difference
/// matters for wave-hold semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTopologyEntry {
    pub probe_name: ProbeName,
    pub kind: String,
    pub mode: ProbeMode,
}

/// Inputs to [`crate::step`]. Each variant maps to exactly one (legal)
/// outbound or inbound RFC-0008 §4.2 event.
#[derive(Debug, Clone)]
pub enum Event {
    // ─────────────────────────────────────────────────────────────────────
    // Agent-local events (synthesized by agent workers, drive agent state)
    // ─────────────────────────────────────────────────────────────────────
    /// Agent has received a Dispatch via long-poll, verified
    /// `target_closure` against the signed manifest, and is about to fire
    /// the switch. Drives `Pending → Activating` and emits a `DispatchAck`
    /// outbound event + a `LocalFireSwitch` effect.
    ///
    /// `target_closure` is the dispatch target validated by the longpoll
    /// worker via `manifest_cache.ensure_for_dispatch` (which fetches
    /// the per-rollout manifest and asserts the manifest's
    /// `host_set[hostname].target_closure` matches the dispatched
    /// value). Carrying it on the event eliminates the reducer's
    /// bootstrap dependency on its own cached `SignedManifestSet`,
    /// which is fed by a separate worker (`agent_manifest_poll`) on
    /// a slower cadence and can be stale when a new rollout's
    /// channel_ref has just been published. RFC-0011 §1 invariant 1:
    /// the longpoll's just-verified value is the single source of
    /// truth at bootstrap time.
    ///
    /// `soak_due_at` is the CP-resolved soak deadline carried verbatim
    /// from the `/v1/agent/dispatch` response
    /// (`DispatchResponse.soak_due_at`, computed by
    /// `runtime::applier::open_rollout` from the manifest's
    /// `rollout_policies[policy].waves[wave_index].soak_minutes`). CP
    /// is the single source of truth for soak resolution per
    /// RFC-0011 §1 invariant 1; the agent's bootstrap reads this
    /// value into `state.soak_due_at` and the convergence-emission
    /// pass-gate reads it back.
    LocalActivate {
        current_closure_at_dispatch: ClosureHash,
        target_closure: ClosureHash,
        received_at: DateTime<Utc>,
        soak_due_at: DateTime<Utc>,
        seq: u64,
    },

    /// Activation worker has begun executing `switch-to-configuration`.
    /// Visibility-only; no state change.
    LocalActivationStarted {
        started_at: DateTime<Utc>,
        switch_method: String,
        seq: u64,
    },

    /// `switch-to-configuration` returned success. Drives
    /// `Activating → Soaking`, sets `current_closure`, resets probe cache.
    LocalActivationCompleted {
        observed_current_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },

    /// Activation pipeline set the profile + bootloader but skipped the
    /// live `switch-to-configuration` because `component` (dbus, systemd,
    /// kernel, init) cannot be safely swapped on a running system
    /// (matches nixos-rebuild's own refusal). The target activates on
    /// next reboot. The host is "soft-staged" — profile is correct, but
    /// `/run/current-system` still points at the pre-switch closure.
    ///
    /// State stays Activating: the activation is not complete until the
    /// host reboots and the new generation takes. When the operator
    /// reboots and the agent restarts, the boot-recovery handshake
    /// observes `current_closure == target_closure` and CP's
    /// `handle_heartbeat` synthesizes `RemoteActivationCompleted`
    /// (LIFT #1; recovery.rs scenario 3). The cascade resumes
    /// automatically post-reboot.
    ///
    /// Visibility-only at the state-machine layer; emits an outbound
    /// `ActivationDeferred` event so CP's event_log + operator queries
    /// can surface the deferred-reboot condition (the host appears
    /// stuck at Activating to the planner; this event tells operators
    /// why).
    LocalActivationDeferred {
        component: String,
        deferred_at: DateTime<Utc>,
        seq: u64,
    },

    /// `switch-to-configuration` returned failure. Drives
    /// `Activating → Failed`. The agent reads `onHealthFailure` from policy
    /// and (if `rollback-and-halt`) immediately fires the rollback in the
    /// same handler — the next event will be `LocalRollbackCompleted`.
    LocalActivationFailed {
        exit_code: i32,
        stderr_tail: String,
        failed_at: DateTime<Utc>,
        seq: u64,
    },

    /// Agent's on-disk probe declarations (RFC-0010 §8). Emitted once
    /// per `LocalActivationCompleted` so the CP can record the
    /// authoritative declared-probe set without reading the agent's
    /// filesystem. Visibility-only; no state change.
    LocalProbeTopologyDeclared {
        probes: Vec<ProbeTopologyEntry>,
        declared_at: DateTime<Utc>,
        seq: u64,
    },

    /// First probe run since activation observed a result (any status).
    /// Stamps `probe_observed_first_at`; soak gate may now consult probe
    /// state. Visibility + gate-enable; no state change.
    LocalProbeObservedFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        observed_at: DateTime<Utc>,
        seq: u64,
    },

    /// A probe ran and produced a result. Updates the probe map. Visibility
    /// + state update on `probes` field; no `HostState` change.
    ///
    /// `sub_results` carries per-control accounting for evidence/custom-
    /// framework probes (one entry per (control, framework, article) tuple
    /// with `effective_mode` + `override_reason`). The reducer does not
    /// consult these — gate decisions key off the aggregate `status` — but
    /// it threads them onto the OutboundAgentEvent so the signed wire
    /// payload + CP event_log preserve the audit trail an auditor needs
    /// to answer "why was control X downgraded?" `None` for non-evidence
    /// probe kinds.
    LocalProbeResult {
        probe_name: ProbeName,
        mode: ProbeMode,
        status: ProbeStatus,
        observed_at: DateTime<Utc>,
        failure_reason: Option<String>,
        sub_results: Option<Vec<ProbeSubResult>>,
        seq: u64,
    },

    /// First `Pass → Fail` (or first-ever `Fail`) for any declared probe.
    /// Stamps `probe_failure_first_at`; sweep window now ticks from this
    /// exact agent-supplied time.
    LocalProbeFailureFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        first_failed_at: DateTime<Utc>,
        seq: u64,
    },

    /// Agent's local sweep timer crossed `HEALTH_FAILURE_THRESHOLD_SECS`.
    /// Drives `Soaking → Failed`. Agent records which policy branch it's
    /// applying (read from the signed manifest); applier follows.
    LocalSustainedFailureCrossed {
        failed_at: DateTime<Utc>,
        sustained_duration_secs: u64,
        failing_probes: Vec<ProbeName>,
        policy_applied: OnHealthFailure,
        seq: u64,
    },

    /// Agent has finished rollback. Drives `Failed → Reverted` and
    /// populates `reverted_to`, `current_closure`.
    LocalRollbackCompleted {
        reverted_to_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },

    /// Agent has re-verified the three Converged invariants
    /// (`soak_due_at` elapsed, all enforce-mode probes Pass, `current ==
    /// target`) and is declaring success. Observe and Disabled probes do
    /// not gate per RFC-0010 §3.3 (ProbeMode docstring, state.rs). Drives
    /// `Soaking → Converged`.
    LocalConvergedReached {
        converged_at: DateTime<Utc>,
        current_closure: ClosureHash,
        seq: u64,
    },

    // ─────────────────────────────────────────────────────────────────────
    // CP-mirror events (synthesized from inbound agent POSTs)
    // ─────────────────────────────────────────────────────────────────────
    /// Mirrors `LocalActivate`. CP receives `DispatchAck` at
    /// `/v1/agent/events`; drives `Pending → Activating` in the mirror.
    RemoteDispatchAck {
        current_closure_at_dispatch: ClosureHash,
        received_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteActivationStarted {
        started_at: DateTime<Utc>,
        switch_method: String,
        seq: u64,
    },

    /// CP-side mirror of `LocalActivationDeferred`. CP receives
    /// `ActivationDeferred` at `/v1/agent/events`; no state change in
    /// the mirror — the host stays Activating until the operator
    /// reboots and LIFT #1's heartbeat synthesis advances state.
    /// Emits a `RemoteAppendEventLog` effect so event_log captures the
    /// deferral for operator queries + replay re-derivability.
    RemoteActivationDeferred {
        component: String,
        deferred_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteActivationCompleted {
        observed_current_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteActivationFailed {
        exit_code: i32,
        stderr_tail: String,
        failed_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteProbeTopologyDeclared {
        probes: Vec<ProbeTopologyEntry>,
        declared_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteProbeObservedFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        observed_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteProbeResult {
        probe_name: ProbeName,
        mode: ProbeMode,
        status: ProbeStatus,
        observed_at: DateTime<Utc>,
        failure_reason: Option<String>,
        sub_results: Option<Vec<ProbeSubResult>>,
        seq: u64,
    },

    RemoteProbeFailureFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        first_failed_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteFailed {
        failed_at: DateTime<Utc>,
        sustained_duration_secs: u64,
        failing_probes: Vec<ProbeName>,
        policy_applied: OnHealthFailure,
        seq: u64,
    },

    RemoteRollbackComplete {
        reverted_to_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },

    RemoteConverged {
        converged_at: DateTime<Utc>,
        current_closure: ClosureHash,
        seq: u64,
    },
}

impl Event {
    /// The monotonic per-(host, rollout) sequence number every event
    /// carries (RFC-0008 §4). Used by the runtime layer for deduplication
    /// and out-of-order detection; the reducer itself just records it on
    /// the state to support `Replay-From` (RFC-0008 §4.3).
    pub fn seq(&self) -> u64 {
        match self {
            Event::LocalActivate { seq, .. }
            | Event::LocalActivationStarted { seq, .. }
            | Event::LocalActivationCompleted { seq, .. }
            | Event::LocalActivationDeferred { seq, .. }
            | Event::LocalActivationFailed { seq, .. }
            | Event::LocalProbeTopologyDeclared { seq, .. }
            | Event::LocalProbeObservedFirst { seq, .. }
            | Event::LocalProbeResult { seq, .. }
            | Event::LocalProbeFailureFirst { seq, .. }
            | Event::LocalSustainedFailureCrossed { seq, .. }
            | Event::LocalRollbackCompleted { seq, .. }
            | Event::LocalConvergedReached { seq, .. }
            | Event::RemoteDispatchAck { seq, .. }
            | Event::RemoteActivationStarted { seq, .. }
            | Event::RemoteActivationCompleted { seq, .. }
            | Event::RemoteActivationDeferred { seq, .. }
            | Event::RemoteActivationFailed { seq, .. }
            | Event::RemoteProbeTopologyDeclared { seq, .. }
            | Event::RemoteProbeObservedFirst { seq, .. }
            | Event::RemoteProbeResult { seq, .. }
            | Event::RemoteProbeFailureFirst { seq, .. }
            | Event::RemoteFailed { seq, .. }
            | Event::RemoteRollbackComplete { seq, .. }
            | Event::RemoteConverged { seq, .. } => *seq,
        }
    }
}
