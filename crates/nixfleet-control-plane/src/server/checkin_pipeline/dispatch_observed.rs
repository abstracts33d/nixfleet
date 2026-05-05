//! Shared `Observed` builder for the dispatch endpoint's gate evaluation.
//!
//! Every gate sees identical inputs at the dispatch endpoint, and a
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
use std::path::Path;

use nixfleet_proto::FleetResolved;
use nixfleet_reconciler::observed::{Observed, Rollout};
use nixfleet_reconciler::{HostRolloutState, RolloutState};

use super::super::state::AppState;

/// Build the per-checkin `Observed` for dispatch-time gate evaluation.
///
/// `rollouts_dir` is `state.rollouts_dir` — the directory CI writes
/// signed rollout manifests into. When `Some`, each active rollout's
/// `disruption_budgets` snapshot is loaded so the budget gate has the
/// frozen membership the reconciler also sees. When `None` (test
/// fixtures, CP without artifact dir), budgets are empty and the
/// budget gate no-ops — same permissive behaviour as
/// `server::reconcile::load_rollout_budgets`.
///
/// Returns a default-empty `Observed` if any DB read fails; callers
/// already handle the "no DB" / "no fleet" cases gracefully.
pub(super) async fn build_observed_for_gates(
    db: &crate::db::Db,
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
    rollouts_dir: Option<&Path>,
) -> Observed {
    let current_rollout_ids: HashSet<String> =
        nixfleet_reconciler::current_rollout_ids(fleet, fleet_resolved_hash);

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

    let mut active_rollouts: Vec<Rollout> = raw
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
            budgets: vec![],
        })
        .collect();

    if let Some(dir) = rollouts_dir {
        for r in active_rollouts.iter_mut() {
            r.budgets = load_budgets_from_manifest(dir, &r.id).await;
        }
    }

    // Compliance failures aggregated by (rollout, host). Same DB query
    // the reconciler tick uses, so the compliance_wave gate sees the
    // same input at both call sites. Permissive on read failure: the
    // gate then no-ops which is the same behaviour as the disabled
    // mode, preserving "missing data is silent" rather than surprising
    // the operator with a hard block.
    let compliance_failures_by_rollout = match db.reports().outstanding_compliance_events_by_rollout() {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "dispatch_observed: outstanding_compliance_events_by_rollout failed; compliance gate no-ops",
            );
            std::collections::HashMap::new()
        }
    };

    Observed {
        active_rollouts,
        compliance_failures_by_rollout,
        ..Default::default()
    }
}

/// Wrapper that pulls the manifest dir from `AppState`. Most callers
/// have AppState handy and shouldn't have to thread the path manually.
pub(super) async fn build_observed_for_gates_from_state(
    state: &AppState,
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
) -> Observed {
    build_observed_for_gates(
        state
            .db
            .as_ref()
            .expect("dispatch_observed: caller already verified db.is_some()"),
        fleet,
        fleet_resolved_hash,
        state.rollouts_dir.as_deref(),
    )
    .await
}

/// Load `disruption_budgets` from a single rollout manifest. Permissive on
/// failure: missing/corrupt manifest → empty budgets → budget gate
/// no-ops for this rollout. Mirrors `server::reconcile::load_rollout_budgets`.
async fn load_budgets_from_manifest(
    dir: &Path,
    rollout_id: &str,
) -> Vec<nixfleet_proto::RolloutBudget> {
    let manifest_path = dir.join(format!("{rollout_id}.json"));
    let bytes = match tokio::fs::read(&manifest_path).await {
        Ok(b) => b,
        Err(err) => {
            tracing::debug!(
                rollout = %rollout_id,
                path = %manifest_path.display(),
                error = %err,
                "dispatch_observed: manifest unavailable; budget gate no-ops",
            );
            return Vec::new();
        }
    };
    match serde_json::from_slice::<nixfleet_proto::RolloutManifest>(&bytes) {
        Ok(m) => m.disruption_budgets,
        Err(err) => {
            tracing::warn!(
                rollout = %rollout_id,
                error = %err,
                "dispatch_observed: manifest parse failed; budget gate no-ops",
            );
            Vec::new()
        }
    }
}
