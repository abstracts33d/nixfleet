//! Per-checkin dispatch decision: query the in-flight pending
//! confirms, consult the wave-staging gate, ask `dispatch::decide_target`
//! for a target, and if the answer is Dispatch, persist a
//! `pending_confirms` row as the idempotency anchor. Helpers staging
//! per-channel host snapshots for the gate live alongside.

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::CheckinRequest;

use super::super::state::AppState;

/// Per-checkin dispatch decision. Failures log + return None: a
/// transient DB hiccup or missing fleet snapshot must not surface as
/// HTTP 500 to the agent (it retries every 60s).
pub(super) async fn dispatch_target_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let db = state.db.as_ref()?;
    let fleet = state.verified_fleet.read().await.clone()?;
    let fleet_resolved_hash = state.fleet_resolved_hash.read().await.clone().or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "dispatch: no fleet_resolved_hash yet; skipping",
        );
        None
    })?;
    let pending_for_host = match db.confirms().pending_confirm_exists(&req.hostname) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "dispatch: pending_confirm_exists query failed",
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
            // Channel must be persisted explicitly (#80 / V005). The
            // host's declared channel is the source of truth; rollout
            // ids no longer encode it (post-#62 they're sha256 hex).
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

/// Snapshot per-host (records, current rollout, wave index) for every
/// host on the given channel. Owned data so the gate iterator has
/// stable lifetimes after the locks drop.
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
                .and_then(|t| t.rollout_id.clone());
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

/// Persist the `pending_confirms` row for a freshly-dispatched
/// target. Returns the target on success, None if the DB write fails
/// (the row is the idempotency anchor — without it the next checkin
/// would re-dispatch, breaking the contract).
#[allow(clippy::too_many_arguments)] // 1:1 dispatch-row shape; pulled apart for clarity at the call site.
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
        .confirms()
        .record_pending_confirm(&crate::db::PendingConfirmInsert {
            hostname,
            rollout_id,
            channel,
            wave: wave_index.unwrap_or(0),
            target_closure_hash: &target.closure_hash,
            target_channel_ref: &target.channel_ref,
            confirm_deadline,
        }) {
        Ok(_) => {
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
                "dispatch: record_pending_confirm failed; returning no target",
            );
            None
        }
    }
}
