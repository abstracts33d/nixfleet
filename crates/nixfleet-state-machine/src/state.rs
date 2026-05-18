//! Per-(rollout, host) reducer state. Maps RFC-0008 §5 `HostRolloutRecord`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use nixfleet_proto::OnHealthFailure;
use serde::{Deserialize, Serialize};

// `RolloutId` lives in `nixfleet-proto` (RFC-0012 §6.3 + D-007 amendment
// `0320c2fa`): a newtype around `"{channel}@{channel_ref}"` with
// constructor discipline analogous to `Verified<T>`. Re-exported from
// here so downstream crates that already `use nixfleet_state_machine::RolloutId`
// keep working without a renaming churn pass.
pub use nixfleet_proto::RolloutId;

pub type ClosureHash = String;
pub type ProbeName = String;

/// 6-state rollout machine per RFC-0008 §3. Replaces the pre-v0.2 9-variant
/// `nixfleet_proto::HostRolloutState` (the `Queued` / `Dispatched` /
/// `ConfirmWindow` / `Healthy` / `Soaked` variants are removed — phases 4-6
/// delete the old enum entirely once the new machine is wired through).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostState {
    /// CP has issued a Dispatch; agent has not yet acked.
    Pending,
    /// Agent acked; switch-to-configuration is firing or has fired pending
    /// confirmation.
    Activating,
    /// Activation pipeline set the profile + bootloader but skipped the
    /// live `switch-to-configuration` because a critical component
    /// (dbus/systemd/kernel/init) cannot be live-swapped on a running
    /// system. The new generation activates on next reboot. **Ordering-
    /// eligible** (host-edges + wave-promotion + advance_current_waves
    /// treat Deferred ≡ Converged for cascade-progression purposes —
    /// the host has done what it can within the rollout step) but
    /// **not health-verified** (probes haven't run against the new
    /// closure; channel-edges stays strict and waits for actual
    /// Converged). On operator reboot, the agent's boot-recovery
    /// handshake observes `current_closure == target_closure` and
    /// CP's `handle_heartbeat` synthesis (LIFT #1) drives
    /// `Deferred → Soaking` via `RemoteActivationCompleted`.
    Deferred,
    /// Agent reports activation succeeded; probes have started; soak window
    /// has not yet elapsed.
    Soaking,
    /// Soak elapsed, probes passing, `current == declared`. **Terminal for
    /// ordering** (successor channels may release).
    Converged,
    /// Sustained probe failure observed by the agent and reported to CP.
    /// Agent has read `onHealthFailure` from the signed manifest and decided
    /// autonomously what comes next (RFC-0008 §4.2 `Failed` event).
    Failed,
    /// Agent has completed rollback to prior closure. Channel-level
    /// quarantine holds the bad SHA.
    Reverted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeStatus {
    Pass,
    Fail,
}

/// Per-probe gate participation (RFC-0010 §3.4). Threaded through every
/// probe event so CP can decide whether to gate on a result without
/// consulting a separate topology table.
///
/// - `Enforce`  — wave gate consults latest result; Fail blocks promotion.
/// - `Observe`  — result recorded in event_log; gate ignores it.
/// - `Disabled` — declared but agent does not run it (operator suppression).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeMode {
    Enforce,
    Observe,
    Disabled,
}

impl Default for ProbeMode {
    /// Conservative default for persisted state that pre-dates the
    /// per-probe `mode` field on `ProbeRecord`: assume `Enforce`. Old
    /// state thus retains its gating semantics on rehydration; the next
    /// probe event from the agent updates the record to the actually-
    /// declared mode per RFC-0010 §3.4.
    fn default() -> Self {
        ProbeMode::Enforce
    }
}

/// Per-control sub-result on a `kind = evidence` probe (RFC-0010 §7.1).
/// `None` aggregate for non-evidence probes; `Some(vec)` for evidence
/// probes — the applier's `probe_failures` co-write iterates `sub_results`
/// to populate one row per failing control_id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSubResult {
    pub control_id: String,
    pub status: ProbeStatus,
    /// Framework label (e.g. "nis2-essential"). Echoes the probe
    /// declaration's `framework` field.
    pub framework: String,
    /// Framework-specific control reference (e.g. "nis2:21(b)").
    pub article: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRecord {
    pub status: ProbeStatus,
    /// Per-probe gate participation per RFC-0010 §3.4. Only
    /// `Enforce`-mode probes contribute to the soak-gate `failing_probes`
    /// builder; `Observe` and `Disabled` record events but do not gate.
    /// `#[serde(default)]` (-> `Enforce`) keeps rehydration safe for
    /// state persisted before this field existed.
    #[serde(default)]
    pub mode: ProbeMode,
    pub last_observed_at: DateTime<Utc>,
    pub last_pass_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

/// Per-(rollout, host) reducer state. Mirrors RFC-0008 §5
/// `HostRolloutRecord`; the persistence schema in Phase 4 serializes this
/// struct.
///
/// Every transition timestamp is agent-supplied (received via the wire and
/// stamped onto the corresponding field by the reducer). `dispatched_at` is
/// the lone exception — it's CP-issued, so CP wallclock is the source of
/// truth there (RFC-0008 §5).
#[derive(Debug, Clone)]
pub struct HostRolloutState {
    pub rollout_id: RolloutId,
    pub hostname: String,
    pub channel: String,
    pub state: HostState,

    // Closures
    pub target_closure: ClosureHash,
    pub current_closure_at_dispatch: Option<ClosureHash>,
    pub current_closure: Option<ClosureHash>,
    pub reverted_to: Option<ClosureHash>,

    // Transition timestamps (agent-supplied unless noted)
    pub dispatched_at: DateTime<Utc>,
    pub dispatch_acked_at: Option<DateTime<Utc>>,
    pub activation_started_at: Option<DateTime<Utc>>,
    pub activation_completed_at: Option<DateTime<Utc>>,
    pub activation_failed_at: Option<DateTime<Utc>>,
    pub probe_observed_first_at: Option<DateTime<Utc>>,
    pub probe_failure_first_at: Option<DateTime<Utc>>,
    pub soak_due_at: Option<DateTime<Utc>>,
    pub converged_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub policy_applied: Option<OnHealthFailure>,
    pub reverted_at: Option<DateTime<Utc>>,

    // Live probe state, by probe name
    pub probes: HashMap<ProbeName, ProbeRecord>,

    /// Monotonic per (hostname, rollout_id). Gaps signal lost events;
    /// out-of-order events are dropped with a warning at the runtime layer.
    pub last_event_seq: u64,
}

impl HostRolloutState {
    /// Construct the initial `Pending` state when CP queues a Dispatch (or
    /// the agent receives one via long-poll on `/v1/agent/dispatch`).
    ///
    /// This is the only legitimate way to bring a `(rollout_id, hostname)`
    /// record into existence. Subsequent transitions go through [`step`].
    pub fn new_pending(
        rollout_id: RolloutId,
        hostname: String,
        channel: String,
        target_closure: ClosureHash,
        dispatched_at: DateTime<Utc>,
        soak_due_at: DateTime<Utc>,
    ) -> Self {
        Self {
            rollout_id,
            hostname,
            channel,
            state: HostState::Pending,
            target_closure,
            current_closure_at_dispatch: None,
            current_closure: None,
            reverted_to: None,
            dispatched_at,
            dispatch_acked_at: None,
            activation_started_at: None,
            activation_completed_at: None,
            activation_failed_at: None,
            probe_observed_first_at: None,
            probe_failure_first_at: None,
            soak_due_at: Some(soak_due_at),
            converged_at: None,
            failed_at: None,
            policy_applied: None,
            reverted_at: None,
            probes: HashMap::new(),
            last_event_seq: 0,
        }
    }
}
