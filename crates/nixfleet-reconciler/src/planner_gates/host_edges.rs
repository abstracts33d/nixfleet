//! Host-edges gate. Per-host DAG within a single rollout:
//! `Edge { gated: A, gates: B }` holds A's dispatch until B is
//! ordering-eligible — Converged (canonical "health-verified at
//! target") OR Deferred (activation staged, live-switch pending
//! operator reboot per RFC-0008 §3 terminal-for-ordering).
//!
//! LOADBEARING: Deferred counts as ordering-eligible. Without this,
//! a single host that hit `DeferredPendingReboot` (framework upgrade
//! touching dbus/systemd/kernel/init) would halt the cascade
//! indefinitely on downstream host-edge dependencies. Deferred is
//! "this host is done participating in the rollout step from an
//! ordering standpoint"; actual health verification (probes, soak)
//! runs once the operator reboots.

use nixfleet_state_machine::HostState;

use crate::planner_gates::GateBlock;
use crate::planner_types::{FleetState, HostId, RolloutId, SignedManifestSet};

pub fn check(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    host: &HostId,
    rollout_id: &RolloutId,
) -> Option<GateBlock> {
    let fleet = manifests.fleet();
    let host_channel = fleet.hosts.get(host).map(|h| h.channel.as_str())?;

    for edge in fleet.edges.iter().filter(|e| e.gated == *host) {
        // Cross-channel guard: silently skip edges where the gating host
        // is on a different channel — that's `channel_edges`'s job, and
        // looking up such a host in this rollout's host_states would
        // always miss and block forever (the bug the old gate's
        // cross-channel filter was added to prevent).
        let same_channel = fleet
            .hosts
            .get(&edge.gates)
            .map(|h| h.channel == host_channel)
            .unwrap_or(false);
        if !same_channel {
            continue;
        }

        // Look up the gating host's state within THIS rollout. Absence
        // means the gating host hasn't started yet → block.
        let key = (rollout_id.clone(), edge.gates.clone());
        let state = fleet_state.host_states.get(&key).map(|s| s.state);
        let ordering_eligible = matches!(
            state,
            Some(HostState::Converged) | Some(HostState::Deferred)
        );
        if !ordering_eligible {
            return Some(GateBlock::HostEdge {
                gating_host: edge.gates.clone(),
            });
        }
    }
    None
}
