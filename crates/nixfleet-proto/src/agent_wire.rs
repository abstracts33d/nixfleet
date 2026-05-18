//! Agent ↔ control-plane wire types. LOADBEARING: within a major version,
//! additions must be backwards-compatible (older consumers serde-ignore unknown
//! fields); bump `PROTOCOL_MAJOR_VERSION` for any breaking change.
//!
//! Phase 8d trimmed the legacy v0.1 checkin / confirm / activate wire shape;
//! Phase 9a then deleted the legacy `/v1/agent/report` surface (`ReportRequest`,
//! `ReportEvent`, `ReportResponse`) — the unified event-driven wire (RFC-0008
//! §4.2 → `runtime/wire.rs` + CP's `server/routes/events.rs`) is now the
//! sole agent→CP event channel.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::RolloutId;
use crate::host_rollout_state::HostRolloutState;

/// Sent in `X-Nixfleet-Protocol`; CP rejects mismatched majors with 426.
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;

pub const PROTOCOL_VERSION_HEADER: &str = "x-nixfleet-protocol";

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRef {
    pub closure_hash: String,
    #[serde(default)]
    pub channel_ref: Option<String>,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGeneration {
    pub closure_hash: String,
}

/// LIFT #3: snapshot of a host's per-rollout state, carried in the
/// HeartbeatResponse when CP detects the agent's reducer is empty but
/// CP holds non-terminal records for the host (typical post-restart
/// shape after LIFT #1 synthesizes the state advance). The agent's
/// boot-recovery handler applies each snapshot to its in-memory
/// HostRolloutState before workers spawn, restoring the cache so
/// probe runners + advance-ticker resume their work post-restart.
///
/// Fields mirror `nixfleet_state_machine::HostRolloutState`'s
/// LOADBEARING set (RFC-0008 §5) — anything the agent's reducer or
/// downstream workers (probe topology, advance-ticker, soak-elapsed
/// detection) need to drive the state forward. Probe state itself is
/// NOT carried: probe runners re-emit `LocalProbeTopologyDeclared`
/// from health-checks.json on startup and probes repopulate via
/// fresh runs. A pre-restart sustained-failure timer (`probe_failure_
/// first_at`) resets across restart; tracked as v0.2.1 polish.
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostRolloutSnapshot {
    pub rollout_id: RolloutId,
    pub hostname: String,
    pub channel: String,
    pub state: HostRolloutState,
    pub target_closure: String,
    #[serde(default)]
    pub current_closure_at_dispatch: Option<String>,
    #[serde(default)]
    pub current_closure: Option<String>,
    pub dispatched_at: DateTime<Utc>,
    #[serde(default)]
    pub dispatch_acked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub activation_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub activation_completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub soak_due_at: Option<DateTime<Utc>>,
    pub last_event_seq: u64,
}
