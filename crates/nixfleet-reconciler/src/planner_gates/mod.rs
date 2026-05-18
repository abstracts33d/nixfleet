//! Planner gates over `&FleetState` (the v0.2 shape, replacing the
//! v0.1 `&Observed`-based gates per RFC-0006 §12).
//!
//! Two design properties the new shape enforces that the old didn't:
//!
//! - **No fail-open defaults.** The old gates carried
//!   `unwrap_or(true)` for missing probe state (see
//!   `Observed::host_probes_passing` docstring). The new gates consult
//!   the reducer state — `HostRolloutState::probe_observed_first_at`
//!   etc. — where absence has explicit meaning (probe hasn't run yet =
//!   soak gate fails closed). RFC-0005 §6.
//!
//! - **Verified manifests only.** Every gate takes `&SignedManifestSet`.
//!   Phase 2's `Verified<T>` newtype graduates from "type exists" to
//!   "type is required on the dispatch path".
//!
//! First-block-wins order matches the old `evaluate_for_host`:
//! `quarantine → channel_edges → wave_promotion → host_edges →
//! disruption_budget → compliance_wave`. Quarantine is FIRST: a hash
//! that just rolled back must stop instantly even if other gates would
//! otherwise hold the host — otherwise the agent re-fetches and re-
//! activates the bad closure on every cycle.

pub mod channel_edges;
pub mod compliance_wave;
pub mod disruption_budget;
pub mod host_edges;
pub mod quarantine;
pub mod wave_promotion;

#[cfg(test)]
mod tests;

use crate::planner_types::{
    ChannelId, ClosureHash, FleetState, HostId, QuarantineSet, RolloutId, SignedManifestSet,
};

/// Reason a host can't be dispatched right now. Variants carry enough
/// detail to render the log line + observability event without re-
/// querying state. The legacy `&Observed`-shaped gate variants from
/// v0.1 are not represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateBlock {
    ChannelEdges {
        predecessor_channel: String,
    },
    WavePromotion {
        host_wave: u32,
        current_wave: u32,
    },
    /// Default-deny variant of the wave-promotion gate: the channel
    /// declares waves but this host is not listed in any of them.
    /// Pre-hardening this case silently passed (`position(...)` returns
    /// None → `?`-early-return → gate returns None → host dispatches).
    /// That's an operator misconfiguration (host should be in a wave;
    /// either an operator forgot to assign it, or a fleet-build bug
    /// dropped it). Default-deny prevents silent dispatch of an
    /// unstaged host.
    HostUnstaged {
        channel: String,
    },
    HostEdge {
        gating_host: String,
    },
    DisruptionBudget {
        in_flight: u32,
        max: u32,
        selector_summary: String,
    },
    ComplianceWave {
        failing_events_count: usize,
        host_wave: u32,
    },
    Quarantined {
        channel: String,
        closure_hash: String,
    },
}

impl GateBlock {
    /// Short human-readable reason for log lines + telemetry payloads.
    pub fn reason(&self) -> String {
        match self {
            GateBlock::ChannelEdges {
                predecessor_channel,
            } => {
                format!("channelEdges predecessor channel '{predecessor_channel}' not converged")
            }
            GateBlock::WavePromotion {
                host_wave,
                current_wave,
            } => {
                format!("wave-promotion: host_wave={host_wave} > current_wave={current_wave}")
            }
            GateBlock::HostUnstaged { channel } => {
                format!(
                    "wave-promotion: channel '{channel}' declares waves but this host is not assigned to any of them — fix the fleet declaration or the host's tag/wave selector"
                )
            }
            GateBlock::HostEdge { gating_host } => {
                format!("host-edge: gating host '{gating_host}' not yet Converged")
            }
            GateBlock::DisruptionBudget {
                in_flight,
                max,
                selector_summary,
            } => format!("disruption-budget: {in_flight}/{max} in flight ({selector_summary})"),
            GateBlock::ComplianceWave {
                failing_events_count,
                host_wave,
            } => format!(
                "compliance-wave: {failing_events_count} outstanding failure(s) on hosts in wave < {host_wave}"
            ),
            GateBlock::Quarantined {
                channel,
                closure_hash,
            } => format!(
                "channel {channel} closure {closure_hash} quarantined (sustained probe failures); push a new closure to clear"
            ),
        }
    }

    /// Stable kebab-case discriminator for telemetry.
    pub fn discriminator(&self) -> &'static str {
        match self {
            GateBlock::ChannelEdges { .. } => "channel-edges",
            GateBlock::WavePromotion { .. } => "wave-promotion",
            GateBlock::HostUnstaged { .. } => "wave-promotion-host-unstaged",
            GateBlock::HostEdge { .. } => "host-edge",
            GateBlock::DisruptionBudget { .. } => "disruption-budget",
            GateBlock::ComplianceWave { .. } => "compliance-wave",
            GateBlock::Quarantined { .. } => "quarantine",
        }
    }
}

/// First block wins. Cheapest-first; quarantine is FIRST for the
/// anti-thrash property (see module docstring).
///
/// `tick_dispatched` is the per-budget counter the planner maintains
/// across a single `plan_next()` call so within-tick over-commit is
/// caught (see `planner_gates::disruption_budget` for the rationale).
/// Empty map = first host in the tick.
//
// Eight parameters because each gate's input set is load-bearing and
// distinct; bundling into a `GateCtx` struct would just move the
// argument count without removing any of it. Clippy's heuristic
// doesn't fit a single dispatch entrypoint of this shape.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_for_dispatch(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    quarantines: &QuarantineSet,
    rollout_id: &RolloutId,
    host: &HostId,
    target_closure: &ClosureHash,
    channel: &ChannelId,
    tick_dispatched: &std::collections::HashMap<disruption_budget::BudgetId, u32>,
) -> Option<GateBlock> {
    if let Some(b) = quarantine::check(quarantines, channel, target_closure) {
        return Some(b);
    }
    if let Some(b) = channel_edges::check(fleet_state, manifests, channel) {
        return Some(b);
    }
    if let Some(b) = wave_promotion::check(fleet_state, manifests, host, rollout_id) {
        return Some(b);
    }
    if let Some(b) = host_edges::check(fleet_state, manifests, host, rollout_id) {
        return Some(b);
    }
    if let Some(b) = disruption_budget::check(fleet_state, rollout_id, host, tick_dispatched) {
        return Some(b);
    }
    if let Some(b) = compliance_wave::check(fleet_state, manifests, host, rollout_id) {
        return Some(b);
    }
    None
}
