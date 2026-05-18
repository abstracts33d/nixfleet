//! Channel-edges gate (new-shape).
//!
//! Predecessor channel's rollout must be terminal-for-ordering before
//! the successor opens. In the new architecture, "terminal-for-ordering"
//! is a direct read of `RolloutSummary.terminal_at` — no more inferring
//! from `host_states.values().all(...)` + `terminal_at` retrofit. The
//! `is_active_for_ordering()` heuristic that the v0.2.0 `c3ab9d75` fix
//! patched goes away with the old gate in Phase 6g.

use nixfleet_proto::RolloutId;

use crate::planner_gates::GateBlock;
use crate::planner_types::{ChannelId, FleetState, SignedManifestSet};

pub fn check(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    successor_channel: &ChannelId,
) -> Option<GateBlock> {
    let fleet = manifests.fleet();

    for edge in fleet
        .channel_edges
        .iter()
        .filter(|e| e.gated == *successor_channel)
    {
        let predecessor = &edge.gates;
        let blocked = predecessor_active(fleet_state, manifests, predecessor);
        if blocked {
            return Some(GateBlock::ChannelEdges {
                predecessor_channel: predecessor.clone(),
            });
        }
    }
    None
}

/// True if the predecessor channel has in-flight work that must finish
/// before the successor advances.
///
/// Source-of-truth precedence:
///
/// 1. No verified manifest for the predecessor channel ⇒ no work declared,
///    return false (gate passes).
/// 2. Manifest present, no rollout row yet for the manifest's current
///    target_ref ⇒ fresh-boot protection: the predecessor's rollout is
///    about to open, block conservatively so the successor cannot race
///    ahead.
/// 3. Manifest present, rollout row exists ⇒ return `terminal_at.is_none()`
///    — in-flight blocks, Terminal passes.
///
/// LOADBEARING: keyed by canonical `RolloutId::new(channel,
/// channel_ref)` (RFC-0008 §6.3), NOT by channel. A channel-level
/// key would conflate "no rollout yet on this channel" with "rollout
/// for this target_ref terminal" — the `terminal_at.is_none()`
/// filter on a channel-keyed map causes the fresh-boot protection to
/// misfire as a permanent block after legitimate completion. Mirror
/// of the planner's rollout-id-keyed OpenRollout predicate at
/// planner.rs's section 1.
fn predecessor_active(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    predecessor: &ChannelId,
) -> bool {
    let Some(rollout_manifest) = manifests.rollouts.get(predecessor) else {
        return false;
    };
    let rollout_id = RolloutId::new(predecessor, &rollout_manifest.inner().channel_ref);
    let Some(summary) = fleet_state.rollouts.get(&rollout_id) else {
        return true;
    };
    summary.terminal_at.is_none()
}
