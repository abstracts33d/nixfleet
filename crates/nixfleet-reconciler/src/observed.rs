//! Internal observed-state types (CONTRACTS.md §VI: non-contract).
//!
//! The CP projects its SQLite state into these structs for each
//! reconcile tick. The reconciler never mutates them.

use crate::host_state::HostRolloutState;
use crate::rollout_state::RolloutState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::str::FromStr;

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
    /// Typed wrapper over the wire string (RFC-0002 §3.1). The
    /// `serde` shim round-trips via [`RolloutState::as_str`] /
    /// [`RolloutState::from_str`] so `observed.json` fixtures stay
    /// byte-identical while callers pattern-match on the enum
    /// without a per-call `from_str` round-trip.
    #[serde(
        serialize_with = "serialize_rollout_state",
        deserialize_with = "deserialize_rollout_state"
    )]
    pub state: RolloutState,
    pub current_wave: usize,
    /// hostname → typed per-host state (RFC-0002 §3.2). Same shim
    /// pattern as `state` above: `host_states` JSON stays a string
    /// map on the wire while in-memory it carries the enum so the
    /// reconciler's pattern-match sites are exhaustive.
    #[serde(
        serialize_with = "serialize_host_states_map",
        deserialize_with = "deserialize_host_states_map"
    )]
    pub host_states: HashMap<String, HostRolloutState>,
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

fn serialize_rollout_state<S: Serializer>(s: &RolloutState, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(s.as_str())
}

fn deserialize_rollout_state<'de, D: Deserializer<'de>>(de: D) -> Result<RolloutState, D::Error> {
    let s = String::deserialize(de)?;
    RolloutState::from_str(&s).map_err(serde::de::Error::custom)
}

fn serialize_host_states_map<S: Serializer>(
    map: &HashMap<String, HostRolloutState>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut m = ser.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        m.serialize_entry(k, v.as_str())?;
    }
    m.end()
}

fn deserialize_host_states_map<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<HashMap<String, HostRolloutState>, D::Error> {
    let raw = HashMap::<String, String>::deserialize(de)?;
    raw.into_iter()
        .map(|(k, v)| {
            HostRolloutState::from_str(&v)
                .map(|s| (k, s))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}
