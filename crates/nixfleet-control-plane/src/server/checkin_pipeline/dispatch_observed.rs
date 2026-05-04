//! Shared `Observed` builder for the dispatch endpoint's gate evaluation.
//!
//! Both the channelEdges gate (today) and the budget / host-edge / wave-
//! promotion / compliance gates (Slice 2) need the same view: active
//! rollouts filtered to the CURRENT fleet's expected rolloutIds, with
//! superseded rows excluded. Centralising the construction here means
//! every gate sees identical inputs at the dispatch endpoint, and a
//! single fix to filtering covers all of them.
//!
//! LOADBEARING: filter by `current_rollout_ids` (derived via
//! `compute_rollout_id_for_channel` against the current fleet snapshot).
//! Without it, a previous-rev `Converged` rollout still in
//! `host_dispatch_state` satisfies the predecessor check for the new
//! successor and channelEdges silently bypasses on the first poll of
//! every release. Same filter as `record_rollouts_gated_by_channel_edges`
//! in the polling layer.
use std::collections::HashSet;
use std::sync::Arc;

use nixfleet_reconciler::observed::{Observed, Rollout};
use nixfleet_reconciler::{HostRolloutState, RolloutState};
use nixfleet_proto::FleetResolved;

/// Build a minimal `Observed` for dispatch-time gate evaluation.
///
/// Returns a default-empty `Observed` if any DB read fails; callers
/// already need to handle the "no DB" / "no fleet" cases gracefully.
pub(super) fn build_observed_for_gates(
    db: &crate::db::Db,
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
) -> Observed {
    let current_rollout_ids: HashSet<String> = fleet
        .channels
        .keys()
        .filter_map(|ch| {
            nixfleet_reconciler::compute_rollout_id_for_channel(fleet, fleet_resolved_hash, ch)
                .ok()
                .flatten()
        })
        .collect();

    let raw = match db.host_dispatch_state().active_rollouts_snapshot() {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "dispatch_observed: active_rollouts_snapshot failed; gates fall back to permissive");
            return Observed::default();
        }
    };
    let superseded: HashSet<String> = db
        .rollouts()
        .superseded_rollout_ids()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let active_rollouts: Vec<Rollout> = raw
        .into_iter()
        .filter(|r| !superseded.contains(&r.rollout_id))
        .filter(|r| current_rollout_ids.contains(&r.rollout_id))
        .map(|snap| Rollout {
            id: snap.rollout_id,
            channel: snap.channel,
            target_ref: snap.target_channel_ref,
            state: RolloutState::Executing,
            current_wave: snap.current_wave as usize,
            host_states: snap
                .host_states
                .iter()
                .filter_map(|(h, s)| {
                    HostRolloutState::from_db_str(s)
                        .ok()
                        .map(|st| (h.clone(), st))
                })
                .collect(),
            last_healthy_since: snap.last_healthy_since,
            // Slice 2 will populate budgets from manifest disk reads.
            budgets: vec![],
        })
        .collect();

    Observed {
        active_rollouts,
        ..Default::default()
    }
}

#[allow(dead_code)]
/// Convenience: same builder but returns Arc'd inputs for cloning into
/// per-checkin futures. Reserved for Slice 2 when manifest budgets need
/// to flow alongside the observed view.
pub(super) fn build_arc(
    db: &crate::db::Db,
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
) -> Arc<Observed> {
    Arc::new(build_observed_for_gates(db, fleet, fleet_resolved_hash))
}
