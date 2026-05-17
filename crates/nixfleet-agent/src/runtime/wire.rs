//! Wire types for the agent's HTTP traffic against CP.
//!
//! These mirror the request / response shapes defined in
//! `nixfleet_control_plane::server::routes::{dispatch,heartbeat,events}`.
//! Hand-mirrored here because they live inside the CP crate (not in
//! `nixfleet-proto`); a future cleanup could move them into
//! `nixfleet-proto::agent_wire` so the two sides share one type.

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutId;
use serde::{Deserialize, Serialize};

/// Body of `GET /v1/agent/dispatch?wait=60` when CP has a queued
/// dispatch for this agent. Empty body / 204 means no work pending.
///
/// `rollout_id` is the canonical `"{channel}@{channel_ref}"` composite
/// (RFC-0012 §6.3 + D-007); serde-transparent so the wire JSON format
/// is identical to the prior `String` shape — only Rust typing
/// changes.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DispatchResponse {
    pub hostname: String,
    pub rollout_id: RolloutId,
    pub target_closure: String,
    pub soak_due_at: DateTime<Utc>,
    pub enqueued_at: DateTime<Utc>,
}

/// Body of `POST /v1/agent/heartbeat`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatRequest {
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<RolloutId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_closure: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub received_at: Option<DateTime<Utc>>,
}

/// Reset signal sent by the applier to the probe worker. Cleared on
/// `Effect::LocalResetProbeCache`. Per-rollout scope.
#[derive(Debug, Clone)]
pub struct ProbeResetCommand {
    pub rollout_id: RolloutId,
}

/// Intent signal sent by the applier to the activation worker. Triggers
/// real systemd-run dispatch. (Stub in 7c — activation worker just emits
/// the corresponding Started/Completed/Failed events directly through
/// the input MPSC.)
#[derive(Debug, Clone)]
pub struct ActivationIntent {
    pub rollout_id: RolloutId,
    pub target_closure: String,
    pub rollback: bool,
}
