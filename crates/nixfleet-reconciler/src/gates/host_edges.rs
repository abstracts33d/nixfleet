//! Host-edges gate — per-host DAG predecessors must reach terminal-for-ordering.
//!
//! Migrated from `crate::host_state::edges::predecessor_blocking`. The
//! reconciler's `handle_wave` already checks this; this is the missing
//! enforcement at the dispatch endpoint (`fleet.edges` is currently
//! empty in the org, but the gap was a footgun waiting for the day
//! someone adds an edge — split-brain enforcement).
//!
//! `Edge { before: A, after: B }` semantics in the existing reconciler
//! code: A is gated on B's completion (A waits for B). The naming is
//! inverted from typical DAG conventions (where `before` is usually the
//! predecessor that runs FIRST), but the existing tests
//! (`predecessor_done_is_not_blocking` etc.) and call sites enshrine
//! this convention. We preserve it here; renaming is a separate
//! follow-up if desired.
//!
//! `Soaked` and `Converged` count as terminal-for-ordering (matching
//! channelEdges semantics — host has cleared its soak, the gating
//! purpose is satisfied).

use crate::host_state::HostRolloutState;

use super::{GateBlock, GateInput};

pub fn check(input: &GateInput) -> Option<GateBlock> {
    // No rollout = no per-host states to gate against. Channel-level
    // gates (channelEdges) hold dispatch in this case until the rollout
    // is recorded.
    let rollout = input.rollout?;

    input
        .fleet
        .edges
        .iter()
        .filter(|e| e.before == input.host)
        .find_map(|e| {
            let other_state = rollout
                .host_states
                .get(&e.after)
                .copied()
                .unwrap_or(HostRolloutState::Queued);
            if matches!(
                other_state,
                HostRolloutState::Soaked | HostRolloutState::Converged
            ) {
                None
            } else {
                Some(GateBlock::HostEdge {
                    gating_host: e.after.clone(),
                })
            }
        })
}
