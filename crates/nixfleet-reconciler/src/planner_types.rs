//! Types consumed/produced by the new planner (RFC-0009 §4.1).
//!
//! These types are NEW alongside the existing `reconcile()` /
//! `gates::*` / `Observed` types — both coexist through Phase 5/6 of
//! the v0.2 fold. Phase 6 wires CP's runtime applier onto the new
//! planner and deletes the old path wholesale per RFC-0009 §12.
//!
//! Dispatch-path enforcement of "verified data only" lands here: the
//! planner accepts `SignedManifestSet`, which carries `Verified<T>`
//! values from Phase 2. A function taking `&SignedManifestSet` is
//! statically guaranteed to be working with cryptographically verified
//! payloads — there is no path that constructs a manifest without
//! going through `nixfleet_reconciler::verify_*`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use nixfleet_proto::{ChannelRef, FleetResolved, RolloutBudget, RolloutManifest};
use nixfleet_state_machine::HostRolloutState;

use crate::verify::Verified;

pub type ChannelId = String;
pub type HostId = String;
pub type ClosureHash = String;

// `RolloutId` is a newtype around `"{channel}@{channel_ref}"`
// (RFC-0012 §6.3 + D-007 amendment `0320c2fa`); lives in nixfleet-proto.
// Re-exported here so callers that already
// `use nixfleet_reconciler::planner_types::RolloutId` keep working.
pub use nixfleet_proto::RolloutId;

/// All verified, freshness-validated manifests the planner needs to
/// reason about the fleet. Constructed by the CP runtime's manifest
/// poll workers after `verify_artifact` / `verify_rollout_manifest`
/// gates have passed.
///
/// Holding a `&SignedManifestSet` is the planner's proof that every
/// manifest in scope has passed the trust contract (RFC-0002 §3 +
/// RFC-0005 §1.5).
pub struct SignedManifestSet {
    pub fleet: Verified<FleetResolved>,
    /// Per-channel signed rollout manifests, keyed by channel name.
    pub rollouts: HashMap<ChannelId, Verified<RolloutManifest>>,
}

impl SignedManifestSet {
    pub fn new(
        fleet: Verified<FleetResolved>,
        rollouts: HashMap<ChannelId, Verified<RolloutManifest>>,
    ) -> Self {
        Self { fleet, rollouts }
    }

    pub fn fleet(&self) -> &FleetResolved {
        self.fleet.inner()
    }
}

/// Aggregated view of per-host state the planner consults. Built by the
/// CP runtime from `host_rollout_records` (Phase 4 schema).
pub struct FleetState {
    /// Per-host state, keyed by `(rollout_id, hostname)`. The reducer
    /// state is the source of truth; this map is a flat view derived
    /// from `host_rollout_records` via
    /// `db::HostRolloutRecords::all_for_rollout` per active rollout.
    pub host_states: HashMap<(RolloutId, HostId), HostRolloutState>,

    /// One active rollout per channel. The mapping is updated by the
    /// applier when `OpenRollout` actions fire.
    pub active_rollout_per_channel: HashMap<ChannelId, RolloutId>,

    pub rollouts: HashMap<RolloutId, RolloutSummary>,

    /// Per-(rollout, host) outstanding enforce-mode probe failure count.
    /// Populated from `db::probe_failures::outstanding_failing_enforce_probes_by_rollout`
    /// at `FleetState` construction time (RFC-0010 §7.2). Read by the
    /// compliance-wave gate; absent entries mean zero failing enforce
    /// probes (RFC-0008 §6 — no fail-open fallback).
    ///
    /// **Phase 9a**: shape is correct, but the projection's source rows
    /// (`probe_failures` table) aren't written until 9b's applier
    /// co-write lands — so the map is always empty in 9a. See
    /// `planner_gates::compliance_wave` for the gate-side note.
    pub outstanding_failing_enforce_probes: HashMap<RolloutId, HashMap<HostId, usize>>,
}

#[derive(Debug, Clone)]
pub struct RolloutSummary {
    pub rollout_id: RolloutId,
    pub channel: ChannelId,
    pub target_ref: ChannelRef,
    pub opened_at: DateTime<Utc>,
    pub terminal_at: Option<DateTime<Utc>>,
    /// Highest wave index for which at least one host has been dispatched.
    /// Used by the wave-promotion gate (`host_wave > current_wave` blocks).
    /// Maintained by the applier; planner reads, never writes.
    pub current_wave: u32,
    /// Disruption-budget snapshot frozen at OpenRollout time. Cross-rollout
    /// in-flight summing matches by selector equality, so reordering the
    /// fleet's budget list does not reshape enforcement
    /// (see gates::disruption_budget comments).
    pub budgets: Vec<RolloutBudget>,
}

/// Per-channel quarantined-closure set. Populated by the
/// `InsertQuarantine` applier (after `RemoteRollbackComplete`); read by
/// the quarantine gate to refuse-to-dispatch a known-bad SHA.
pub type QuarantineSet = HashMap<ChannelId, HashSet<ClosureHash>>;

/// Planner outputs. The applier interprets each variant against real
/// I/O (DB writes, queued HTTP responses, metrics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAction {
    /// A new channel ref has arrived and the planner is opening the
    /// per-host record set for it. Applier inserts the rollout into
    /// `rollouts` + creates `host_rollout_records` rows.
    OpenRollout {
        rollout_id: RolloutId,
        channel: ChannelId,
        target_ref: ChannelRef,
    },

    /// Queue a Dispatch for a single host on the agent's next long-poll
    /// to `/v1/agent/dispatch`. Per RFC-0008 §4.1 the payload is
    /// advisory; agent cross-checks against signed manifest.
    QueueDispatch {
        host: HostId,
        rollout: RolloutId,
        target_closure: ClosureHash,
        soak_due_at: DateTime<Utc>,
    },

    // No `MarkChannelTerminal` variant: terminal transitions are driven
    // by the rollout reducer (RFC-0012 §3) via
    // `RolloutEffect::RecordRolloutTransition`, not by the planner.
    //
    // No `ClearStaleQuarantine` variant: quarantines are append-only
    // under the derived-view discipline (RFC-0012 §6.4). Operator-
    // driven clearance would land as an explicit event matching the
    // `OperatorClearance` shape.
    /// Record that a channel was halted (operator-visible status hint).
    RecordHaltLifted { channel: ChannelId },

    /// A host was eligible for dispatch but a gate blocked it. Applier
    /// appends an `event_log` entry with `kind = 'gate_decision'` and the
    /// supplied reason. Does NOT queue any agent-visible work.
    DeferDispatch {
        host: HostId,
        rollout: RolloutId,
        gate: &'static str,
        reason: String,
    },
}

/// Re-export of the existing rich gate-block enum. The new planner_gates
/// reuse the same variant set; one canonical type for "why a dispatch
/// didn't fire" prevents drift between dispatch-time telemetry and
/// reconcile-time telemetry.
pub use crate::planner_gates::GateBlock;
