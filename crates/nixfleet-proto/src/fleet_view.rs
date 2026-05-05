//! Read-model views of fleet state served by the CP for operator-facing
//! consumers (`/v1/hosts`, CLI, metrics exporter). One `HostStatusEntry`
//! per declared host; outstanding-event counts apply resolution-by-
//! replacement (events from older rollouts are considered resolved).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::HostRolloutState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostStatusEntry {
    pub hostname: String,
    pub channel: String,
    #[serde(default)]
    pub declared_closure_hash: Option<String>,
    #[serde(default)]
    pub current_closure_hash: Option<String>,
    #[serde(default)]
    pub pending_closure_hash: Option<String>,
    #[serde(default)]
    pub last_checkin_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_rollout_id: Option<String>,
    pub converged: bool,
    pub outstanding_compliance_failures: usize,
    pub outstanding_runtime_gate_errors: usize,
    pub verified_event_count: usize,
    /// Reported by the agent at every checkin. Surfaces crash-loops that
    /// don't show up as offline (rapid restart, low value despite recent
    /// `last_checkin_at`).
    #[serde(default)]
    pub last_uptime_secs: Option<u64>,
    /// Per-host rollout state machine position for the channel's CURRENT
    /// rolloutId (computed from verified_fleet, not the agent-reported
    /// last_rollout_id which may be stale after a fresh deploy). `None`
    /// when no DB row exists yet for the current rollout — a freshly
    /// opened rollout shows None until the host transitions.
    #[serde(default)]
    pub rollout_state: Option<HostRolloutState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostsResponse {
    pub hosts: Vec<HostStatusEntry>,
}
