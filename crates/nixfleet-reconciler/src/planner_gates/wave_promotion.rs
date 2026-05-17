//! Wave-promotion gate (new-shape). Host's wave index must not exceed
//! the rollout's `current_wave`. Wave index comes from the verified
//! `FleetResolved.waves[channel]` (positional). `current_wave` lives on
//! `RolloutSummary` and is maintained by the applier.

use crate::planner_gates::GateBlock;
use crate::planner_types::{FleetState, HostId, RolloutId, SignedManifestSet};

pub fn check(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    host: &HostId,
    rollout_id: &RolloutId,
) -> Option<GateBlock> {
    let fleet = manifests.fleet();
    let channel = fleet.hosts.get(host).map(|h| h.channel.as_str())?;

    // Position the host in the wave list.
    let host_wave = fleet.waves.get(channel).and_then(|waves| {
        waves
            .iter()
            .position(|w| w.hosts.iter().any(|h| h == host))
            .map(|i| i as u32)
    })?;

    let current_wave = fleet_state
        .rollouts
        .get(rollout_id)
        .map(|r| r.current_wave)
        .unwrap_or(0);

    if host_wave > current_wave {
        Some(GateBlock::WavePromotion {
            host_wave,
            current_wave,
        })
    } else {
        None
    }
}
