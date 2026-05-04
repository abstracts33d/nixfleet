//! Observed-state types. CP projects SQLite state into these per tick;
//! reconciler never mutates them.

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
    /// resolution-by-replacement so events under a superseded rollout
    /// don't gate the new one.
    #[serde(default)]
    pub compliance_failures_by_rollout: HashMap<String, HashMap<String, usize>>,
    /// Last `RolloutDeferred` the CP successfully journalled per channel.
    /// The reconciler consults this and only emits a fresh `RolloutDeferred`
    /// when (target_ref, blocked_by) would change — without this debounce,
    /// every reconcile tick on a blocked channel would pollute the journal
    /// with an identical line.
    #[serde(default)]
    pub last_deferrals: HashMap<String, DeferralRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeferralRecord {
    pub target_ref: String,
    pub blocked_by: String,
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
    /// Serde shim: wire is string, in-memory is typed enum.
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
    /// `now - last_healthy_since[host] >= wave.soak_minutes` ⇒ soaked.
    /// Hosts not in Healthy are absent.
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
        m.serialize_entry(k, v.as_db_str())?;
    }
    m.end()
}

fn deserialize_host_states_map<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<HashMap<String, HostRolloutState>, D::Error> {
    let raw = HashMap::<String, String>::deserialize(de)?;
    raw.into_iter()
        .map(|(k, v)| {
            HostRolloutState::from_db_str(&v)
                .map(|s| (k, s))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}
