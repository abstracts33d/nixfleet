//! `fleet.resolved.json`. Produced by CI's Nix eval, consumed by the CP
//! and (fallback path) agents. Byte-identical JCS bytes across Nix + Rust.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FleetResolved {
    pub schema_version: u32,
    pub hosts: HashMap<String, Host>,
    pub channels: HashMap<String, Channel>,
    #[serde(default)]
    pub rollout_policies: HashMap<String, RolloutPolicy>,
    pub waves: HashMap<String, Vec<Wave>>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub disruption_budgets: Vec<DisruptionBudget>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub system: String,
    pub tags: Vec<String>,
    pub channel: String,
    #[serde(default)]
    pub closure_hash: Option<String>,
    #[serde(default)]
    pub pubkey: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub rollout_policy: String,
    pub reconcile_interval_minutes: u32,
    /// MINUTES (despite missing `_minutes` suffix — kept for wire-compat).
    /// Convert via [`Channel::freshness_window_duration`].
    pub freshness_window: u32,
    pub signing_interval_minutes: u32,
    pub compliance: Compliance,
}

impl Channel {
    /// `freshness_window` is MINUTES; this helper avoids the
    /// `Duration::from_secs(raw)` 60× landmine.
    pub fn freshness_window_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.freshness_window as u64 * 60)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Compliance {
    pub frameworks: Vec<String>,
    /// `disabled` / `permissive` / `enforce`. Default `enforce`.
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RolloutPolicy {
    pub strategy: String,
    pub waves: Vec<PolicyWave>,
    #[serde(default)]
    pub health_gate: HealthGate,
    pub on_health_failure: OnHealthFailure,
}

/// Recovery action when a host fails its health gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnHealthFailure {
    /// Stop advancing; failed host stays Failed pending operator action.
    Halt,
    /// Roll the failed host back to its previous closure, then halt.
    RollbackAndHalt,
}

impl std::fmt::Display for OnHealthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OnHealthFailure::Halt => "halt",
            OnHealthFailure::RollbackAndHalt => "rollback-and-halt",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PolicyWave {
    pub selector: Selector,
    pub soak_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Selector {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tags_any: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthGate {
    // GOTCHA: Nix emits `"healthGate": {}` when no inner constraints set; skip-on-None preserves that empty-object shape (other Option fields here serialize None as null).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub systemd_failed_units: Option<SystemdFailedUnits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compliance_probes: Option<ComplianceProbes>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemdFailedUnits {
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceProbes {
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Wave {
    pub hosts: Vec<String>,
    pub soak_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisruptionBudget {
    pub hosts: Vec<String>,
    #[serde(default)]
    pub max_in_flight: Option<u32>,
    #[serde(default)]
    pub max_in_flight_pct: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub schema_version: u32,
    #[serde(default)]
    pub signed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ci_commit: Option<String>,
    pub signature_algorithm: String,
}
