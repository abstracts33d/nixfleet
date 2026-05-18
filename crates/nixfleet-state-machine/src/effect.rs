//! Reducer outputs. Descriptions of side effects, not executions.
//!
//! Per RFC-0006 §9: 4 agent-only variants, 5 CP-only variants, 3 shared.
//! The agent applier (Phase 7) handles `Local*` + shared; the CP applier
//! (Phase 6) handles `Remote*` + shared. The compiler's exhaustiveness
//! check guarantees every variant has an arm in its applier — adding a
//! variant fails the build at every applier that doesn't account for it.

use chrono::{DateTime, Utc};
use nixfleet_proto::OnHealthFailure;

use crate::event::ProbeTopologyEntry;
use crate::state::{
    ClosureHash, HostState, ProbeMode, ProbeName, ProbeStatus, ProbeSubResult, RolloutId,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    // ─────────────────────────────────────────────────────────────────────
    // Agent-only effects (CP applier returns Error if it sees these)
    // ─────────────────────────────────────────────────────────────────────
    /// Fire `switch-to-configuration` on `target`. Agent applier delegates
    /// to the `activation` module which detaches via `systemd-run` per the
    /// agent-process-restart contract.
    LocalFireSwitch {
        rollout_id: RolloutId,
        target: ClosureHash,
    },

    /// Fire `switch-to-configuration` on `closure` (rollback target read
    /// from `current_closure_at_dispatch`). Agent decides this without a
    /// CP signal — manifest's `onHealthFailure` is the single signed
    /// source of truth (RFC-0005 §4.1).
    LocalFireRollbackTo {
        rollout_id: RolloutId,
        closure: ClosureHash,
    },

    /// Drop the in-memory probe cache for this `(rollout, host)` pair.
    /// Emitted on `LocalActivationCompleted` so stale `Pass` results from
    /// the prior closure cannot satisfy the new rollout's gates.
    LocalResetProbeCache { rollout_id: RolloutId },

    /// Emit an outbound event to CP via `POST /v1/agent/events`. `durable`
    /// requests on-disk queuing before the network call so a crash between
    /// the local state change and the POST is recoverable on restart
    /// (RFC-0005 §9.7 — open question; default policy decided in Phase 7).
    ///
    /// `rollout_id` carries the rollout this event belongs to so the agent
    /// applier can persist the outbound queue entry against the correct
    /// `(host, rollout, seq)` triple without consulting a side channel.
    /// Closes Phase 7's `enrich_effect_with_rollout` stopgap.
    LocalEmitEvent {
        rollout_id: RolloutId,
        payload: OutboundAgentEvent,
        durable: bool,
    },

    // ─────────────────────────────────────────────────────────────────────
    // CP-only effects (agent applier returns Error if it sees these)
    // ─────────────────────────────────────────────────────────────────────
    /// Queue a Dispatch for the agent's next long-poll on
    /// `/v1/agent/dispatch`. Pull-only — CP never opens a connection
    /// (RFC-0005 §2.1).
    RemoteQueueDispatch {
        host: String,
        rollout_id: RolloutId,
        target_closure: ClosureHash,
        soak_due_at: DateTime<Utc>,
    },

    /// Mark a closure as quarantined on a channel after a `RollbackComplete`
    /// arrives. Subsequent dispatches refuse this closure on this channel.
    RemoteInsertQuarantine {
        channel: String,
        closure: ClosureHash,
    },

    // No `RemoteClearStaleQuarantine` variant: quarantines are
    // append-only under the derived-view discipline (RFC-0008 §6.4).
    // Operator-driven clearance, if ever needed,
    // becomes a future explicit event (mirrors `OperatorClearance`).
    /// Persist a fresh `host_rollout_records` row when CP first dispatches
    /// to a host for a new rollout (Phase 4 schema).
    RemoteOpenRolloutRecord {
        rollout_id: RolloutId,
        channel: String,
        host: String,
    },

    /// Append an inbound agent event to the audit log
    /// (RFC-0005 §4.3 + the broader event-log pattern — every state
    /// mutation, gate decision, manifest poll lands here too).
    RemoteAppendEventLog {
        host: String,
        rollout_id: RolloutId,
        payload: OutboundAgentEvent,
    },

    // ─────────────────────────────────────────────────────────────────────
    // Shared effects (both runtimes handle these)
    // ─────────────────────────────────────────────────────────────────────
    /// Record a state transition (from, to, at) for the event log + status
    /// API. Emitted on every legal `HostState` change.
    RecordTransition {
        host: String,
        rollout_id: RolloutId,
        from: HostState,
        to: HostState,
        at: DateTime<Utc>,
    },

    EmitMetric {
        name: &'static str,
        labels: Vec<(&'static str, String)>,
        value: f64,
    },

    EmitLog {
        level: LogLevel,
        target: &'static str,
        message: &'static str,
        fields: Vec<(&'static str, String)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Outbound wire payloads (POST `/v1/agent/events`). Defined here for the
/// reducer's `LocalEmitEvent` effect; Phase 6/7 lifts these into
/// `nixfleet-proto::agent_wire` once the HTTP routes are wired.
#[derive(Debug, Clone, PartialEq)]
pub enum OutboundAgentEvent {
    DispatchAck {
        current_closure_at_dispatch: ClosureHash,
        received_at: DateTime<Utc>,
        seq: u64,
    },
    ActivationStarted {
        started_at: DateTime<Utc>,
        switch_method: String,
        seq: u64,
    },
    ActivationCompleted {
        observed_current_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },
    ActivationFailed {
        exit_code: i32,
        stderr_tail: String,
        failed_at: DateTime<Utc>,
        seq: u64,
    },
    /// LIFT #2 (RFC-0005 §4.2): live activation skipped because
    /// `component` (dbus/systemd/kernel/init) cannot be live-swapped on
    /// a running system. Profile + bootloader updated; next reboot
    /// completes the activation. Host stays at Activating until the
    /// operator reboots; CP's handle_heartbeat (LIFT #1) synthesizes
    /// the completion on the agent's next boot-recovery handshake.
    /// Visibility-only at the wire level — replaces the pre-LIFT #2
    /// fake-`ActivationCompleted` that lied with `exit_code = 0` and a
    /// stale `observed_current_closure`.
    ActivationDeferred {
        component: String,
        deferred_at: DateTime<Utc>,
        seq: u64,
    },
    ProbeTopologyDeclared {
        probes: Vec<ProbeTopologyEntry>,
        declared_at: DateTime<Utc>,
        seq: u64,
    },
    ProbeObservedFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        observed_at: DateTime<Utc>,
        seq: u64,
    },
    ProbeResult {
        probe_name: ProbeName,
        mode: ProbeMode,
        status: ProbeStatus,
        observed_at: DateTime<Utc>,
        failure_reason: Option<String>,
        /// `None` for non-evidence probes; `Some(vec)` for evidence
        /// probes, carrying per-control sub-results. The applier's
        /// `probe_failures` co-write iterates this to populate one row
        /// per failing control (RFC-0007 §7.1 + §7.2).
        sub_results: Option<Vec<ProbeSubResult>>,
        seq: u64,
    },
    ProbeFailureFirst {
        probe_name: ProbeName,
        mode: ProbeMode,
        first_failed_at: DateTime<Utc>,
        seq: u64,
    },
    Failed {
        failed_at: DateTime<Utc>,
        sustained_duration_secs: u64,
        failing_probes: Vec<ProbeName>,
        policy_applied: OnHealthFailure,
        seq: u64,
    },
    RollbackComplete {
        reverted_to_closure: ClosureHash,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },
    Converged {
        converged_at: DateTime<Utc>,
        current_closure: ClosureHash,
        seq: u64,
    },
}
