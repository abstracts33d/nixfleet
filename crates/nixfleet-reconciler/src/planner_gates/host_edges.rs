//! Host-edges gate (new-shape). Per-host DAG within a single rollout:
//! `Edge { gated: A, gates: B }` holds A's dispatch until B is
//! Converged (terminal-for-ordering in the new 6-state machine — Soaked
//! is gone, only Converged counts).

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
        let terminal = matches!(state, Some(HostState::Converged));
        if !terminal {
            return Some(GateBlock::HostEdge {
                gating_host: edge.gates.clone(),
            });
        }
    }
    None
}
