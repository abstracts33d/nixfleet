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

    // The gated host's channel — used to skip cross-channel edges below.
    // Cross-channel host ordering is what `channelEdges` is for; allowing
    // host edges across channels would silently brick the gated host
    // because the gate operates within a single rollout's `host_states`,
    // which only contains hosts on the same channel.
    let host_channel = input
        .fleet
        .hosts
        .get(input.host)
        .map(|h| h.channel.as_str())?;

    input
        .fleet
        .edges
        .iter()
        .filter(|e| e.before == input.host)
        .filter(|e| {
            // Cross-channel guard. Without this, an edge like
            // `Edge { before: krach (stable), after: lab (edge) }` would
            // look up `lab` in the stable rollout's host_states, find
            // nothing, default to `Queued`, and block krach forever.
            // mkFleet should validate this at fleet-eval time too — the
            // CP-side guard is defence in depth (and protects already-
            // built fleet snapshots that pre-date the validation).
            //
            // Cross-channel ordering is `channelEdges`'s job, not host
            // edges'. Silently skipping mismatched edges is preferable
            // to bricking the gated host — operators can detect the
            // misconfiguration via fleet validation, not a silent halt.
            input
                .fleet
                .hosts
                .get(&e.after)
                .map(|h| h.channel == host_channel)
                .unwrap_or(false)
        })
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
