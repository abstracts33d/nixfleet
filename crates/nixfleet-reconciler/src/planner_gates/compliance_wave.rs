//! Compliance-wave gate. Earlier-wave hosts with outstanding evidence
//! failures hold later-wave dispatch.
//!
//! **Phase 9a state: STUB**. Reads from
//! `FleetState.outstanding_failing_enforce_probes` (populated in 9b once
//! the `probe_failures` projection writer lands). Until 9b, the source
//! map is empty and this gate is effectively pass-through. Per RFC-0007
//! §7.2 the gate's input pipeline is being rebuilt against the new
//! probe-result model; the shape is correct in 9a, data starts flowing
//! in 9b.

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

    let host_wave_idx = fleet
        .waves
        .get(host_channel)
        .and_then(|waves| waves.iter().position(|w| w.hosts.iter().any(|h| h == host)));
    let host_wave = match host_wave_idx {
        Some(0) | None => return None, // wave 0 / unwaved: no earlier wave to gate on
        Some(n) => n,
    };

    let waves = fleet.waves.get(host_channel)?;
    let per_host = fleet_state
        .outstanding_failing_enforce_probes
        .get(rollout_id)?;

    let mut failing_count: usize = 0;
    for wave in waves.iter().take(host_wave) {
        for h in &wave.hosts {
            if let Some(&n) = per_host.get(h)
                && n > 0
            {
                failing_count += n;
            }
        }
    }

    if failing_count > 0 {
        Some(GateBlock::ComplianceWave {
            failing_events_count: failing_count,
            host_wave: host_wave as u32,
        })
    } else {
        None
    }
}
