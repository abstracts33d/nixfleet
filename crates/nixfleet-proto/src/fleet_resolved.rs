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
    /// Cross-channel ordering: a `before` channel must reach Converged
    /// before any new rollout opens on the `after` channel. RFC-0002 §4.3
    /// — within-channel coordination uses `edges`; channel-level uses this.
    /// Cycles are rejected at mkFleet eval time.
    #[serde(default)]
    pub channel_edges: Vec<ChannelEdge>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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

impl Selector {
    /// Match a single host. Mirrors `lib/mk-fleet.nix:resolveSelector` —
    /// any rule that fires (all / hosts / channel / tags-all / tags-any)
    /// matches; sub-selector composition (and / not) is mkFleet-only and
    /// not exposed in the wire format.
    pub fn matches(&self, host_name: &str, host: &Host) -> bool {
        if self.all {
            return true;
        }
        if !self.hosts.is_empty() && self.hosts.iter().any(|h| h == host_name) {
            return true;
        }
        if let Some(ch) = &self.channel {
            if &host.channel == ch {
                return true;
            }
        }
        if !self.tags.is_empty() && self.tags.iter().all(|t| host.tags.contains(t)) {
            return true;
        }
        if !self.tags_any.is_empty() && self.tags_any.iter().any(|t| host.tags.contains(t)) {
            return true;
        }
        false
    }

    /// Resolve to the matching host names. Order is `fleet.hosts`'s natural
    /// iteration; callers that need a stable ordering should sort.
    pub fn resolve<'a, I: IntoIterator<Item = (&'a String, &'a Host)>>(
        &self,
        hosts: I,
    ) -> Vec<String> {
        hosts
            .into_iter()
            .filter(|(n, h)| self.matches(n, h))
            .map(|(n, _)| n.clone())
            .collect()
    }
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

/// Cross-channel ordering edge. `before` channel must converge before any
/// rollout opens on `after`. "Converge" = the most-recent rollout on `before`
/// reached terminal state `converged`. If `before` has never had a rollout,
/// the gate is open (no rollout to wait for). Validated at mkFleet eval time:
/// both channels must exist, no cycles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelEdge {
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisruptionBudget {
    /// Tag-driven selector resolved at reconcile time so adding/removing
    /// hosts under a tag doesn't require re-signing fleet.resolved.
    pub selector: Selector,
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
