//! Per-checkin dispatch: gate, decide, persist operational + audit rows.

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::CheckinRequest;

use super::super::state::AppState;

/// Failures log + return None; transient errors must not surface as 500 to the agent.
pub(super) async fn dispatch_target_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let db = state.db.as_ref()?;
    let snap = state.verified_fleet.read().await.clone()?;
    let fleet = snap.fleet;
    let fleet_resolved_hash = snap.fleet_resolved_hash;
    let pending_for_host = match db
        .host_dispatch_state()
        .pending_dispatch_exists(&req.hostname)
    {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "dispatch: pending_dispatch_exists query failed",
            );
            return None;
        }
    };

    if super::wave_gate::wave_gate_blocks_dispatch(state, req, &fleet).await {
        return None;
    }

    let decision = crate::dispatch::decide_target(
        &req.hostname,
        req,
        &fleet,
        &fleet_resolved_hash,
        pending_for_host,
        now,
        state.confirm_deadline_secs as u32,
    );
    match decision {
        crate::dispatch::Decision::Dispatch {
            target,
            rollout_id,
            wave_index,
        } => {
            // Persist channel explicitly: content-addressed rolloutIds don't encode it.
            let channel = fleet
                .hosts
                .get(&req.hostname)
                .map(|h| h.channel.clone())
                .unwrap_or_default();
            record_dispatched_target(
                db,
                &req.hostname,
                &rollout_id,
                &channel,
                wave_index,
                target,
                state,
                now,
            )
        }
        other => {
            tracing::debug!(
                target: "dispatch",
                hostname = %req.hostname,
                decision = ?other,
                "dispatch: no target",
            );
            None
        }
    }
}

/// Owned per-host snapshot so gate iterator outlives the lock guards.
pub(super) async fn stage_channel_hosts(
    state: &AppState,
    fleet: &nixfleet_proto::FleetResolved,
    channel_name: &str,
) -> Vec<(
    String,
    Vec<crate::server::ReportRecord>,
    Option<String>,
    Option<u32>,
)> {
    let reports_guard = state.host_reports.read().await;
    let checkins_guard = state.host_checkins.read().await;
    fleet
        .hosts
        .iter()
        .filter(|(_, h)| h.channel == channel_name)
        .map(|(n, _)| {
            let buf: Vec<crate::server::ReportRecord> = reports_guard
                .get(n)
                .map(|q| q.iter().cloned().collect())
                .unwrap_or_default();
            let cur_rollout = checkins_guard
                .get(n)
                .and_then(|c| c.checkin.last_evaluated_target.as_ref())
                .map(|t| t.rollout_id.clone());
            let wave_idx = wave_index_for(fleet, channel_name, n);
            (n.clone(), buf, cur_rollout, wave_idx)
        })
        .collect()
}

pub(super) fn wave_index_for(
    fleet: &nixfleet_proto::FleetResolved,
    channel_name: &str,
    hostname: &str,
) -> Option<u32> {
    fleet.waves.get(channel_name).and_then(|waves| {
        waves
            .iter()
            .position(|w| w.hosts.iter().any(|h| h == hostname))
            .map(|i| i as u32)
    })
}

/// Returns None on DB failure: the row is the idempotency anchor.
#[allow(clippy::too_many_arguments)]
fn record_dispatched_target(
    db: &crate::db::Db,
    hostname: &str,
    rollout_id: &str,
    channel: &str,
    wave_index: Option<u32>,
    target: nixfleet_proto::agent_wire::EvaluatedTarget,
    state: &AppState,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let confirm_deadline = now + chrono::Duration::seconds(state.confirm_deadline_secs);
    match db
        .host_dispatch_state()
        .record_dispatch(&crate::db::DispatchInsert {
            hostname,
            rollout_id,
            channel,
            wave: wave_index.unwrap_or(0),
            target_closure_hash: &target.closure_hash,
            target_channel_ref: &target.channel_ref,
            confirm_deadline,
        }) {
        Ok(()) => {
            tracing::info!(
                target: "dispatch",
                hostname = %hostname,
                rollout = %rollout_id,
                target_closure = %target.closure_hash,
                confirm_deadline = %confirm_deadline.to_rfc3339(),
                "dispatch: target issued",
            );
            Some(target)
        }
        Err(err) => {
            tracing::warn!(
                hostname = %hostname,
                rollout = %rollout_id,
                error = %err,
                "dispatch: record_dispatch failed; returning no target",
            );
            None
        }
    }
}
