//! Internal observed-state types (CONTRACTS.md §VI: non-contract).
//!
//! The CP projects its SQLite state into these structs for each
//! reconcile tick. The reconciler never mutates them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Observed {
    pub channel_refs: HashMap<String, String>,
    pub last_rolled_refs: HashMap<String, String>,
    pub host_state: HashMap<String, HostState>,
    pub active_rollouts: Vec<Rollout>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostState {
    pub online: bool,
    #[serde(default)]
    pub current_generation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rollout {
    pub id: String,
    pub channel: String,
    pub target_ref: String,
    pub state: String,
    pub current_wave: usize,
    pub host_states: HashMap<String, String>,
    /// When each host most recently entered Healthy. Step 3
    /// (reconciler arm, RFC-0002 §3.2 Healthy → Soaked transition)
    /// consults `now - last_healthy_since[host] >= wave.soak_minutes`
    /// to decide whether the host has soaked. Hosts not in Healthy
    /// are absent from the map. `#[serde(default)]` keeps file-
    /// backed `observed.json` fixtures (which predate this field)
    /// deserialising cleanly.
    #[serde(default)]
    pub last_healthy_since: HashMap<String, DateTime<Utc>>,
}
