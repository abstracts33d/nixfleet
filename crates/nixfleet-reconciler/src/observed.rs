//! Internal observed-state types. CP projects its SQLite state into
//! these per reconcile tick; reconciler never mutates them.

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
    /// `[rollout_id][host] → count`. Per-rollout grouping enforces
    /// resolution-by-replacement: events under R₀ don't contaminate
    /// R₁'s gate decision once the host moves on. Drives
    /// `Action::WaveBlocked` under enforce mode.
    #[serde(default)]
    pub compliance_failures_by_rollout: HashMap<String, HashMap<String, usize>>,
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
    /// Serde shim keeps `observed.json` fixtures byte-identical
    /// while in-memory we get exhaustive pattern matching.
    #[serde(
        serialize_with = "serialize_rollout_state",
        deserialize_with = "deserialize_rollout_state"
    )]
    pub state: RolloutState,
    pub current_wave: usize,
    #[serde(
        serialize_with = "serialize_host_states_map",
        deserialize_with = "deserialize_host_states_map"
    )]
    pub host_states: HashMap<String, HostRolloutState>,
    /// `now - last_healthy_since[host] >= wave.soak_minutes` →
    /// host has soaked. Hosts not in Healthy are absent.
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
