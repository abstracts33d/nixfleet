//! Live cross-channel deferral state for the dashboard.
//!
//! Computed fresh per request from `(channel_edges, active_rollouts,
//! channel_refs)` so the panel reflects domain truth, not the journal-
//! debounce snapshot. The CP's in-memory `last_deferrals` is intentionally
//! NOT consulted here — debounce state and observability state answer
//! different questions and must not converge on the same source.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;

use super::super::state::AppState;
use crate::observed_projection;

/// `GET /v1/deferrals` — list channels currently held by `channelEdges`.
///
/// Each entry: `{ channel, target_ref, blocked_by, reason }`. Empty list
/// when nothing's deferred. Cross-channel coordination is RFC-0002 §4.3
/// — see `predecessor_channel_blocking` in nixfleet-reconciler.
pub(in crate::server) async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let snapshot = match state.verified_fleet.read().await.clone() {
        Some(s) => s,
        // No verified-fleet snapshot yet: dashboard treats this as "no
        // deferrals known" rather than 503; the channels-observed counter
        // already surfaces this state separately.
        None => return Ok(empty_body()),
    };
    let fleet = &snapshot.fleet;

    let channel_refs = state.channel_refs_cache.read().await.refs.clone();
    let checkins = state.host_checkins.read().await.clone();
    let dispatch_snapshot = match state
        .db
        .as_deref()
        .map(|db| db.host_dispatch_state().active_rollouts_snapshot())
    {
        Some(Ok(v)) => v,
        Some(Err(err)) => {
            tracing::warn!(error = %err, "deferrals: active_rollouts_snapshot failed");
            Vec::new()
        }
        None => Vec::new(),
    };
    let superseded: std::collections::HashSet<String> = state
        .db
        .as_deref()
        .map(|db| db.rollouts().superseded_rollout_ids())
        .and_then(|r| r.ok())
        .unwrap_or_default()
        .into_iter()
        .collect();
    // LOADBEARING: filter to the CURRENT fleet's expected rolloutIds so a
    // stale Converged rollout from the previous rev doesn't satisfy the
    // predecessor check and hide the actual deferral. Same filter as
    // the polling layer's `record_rollouts_gated_by_channel_edges` —
    // the two stay symmetric so dashboard observability matches the
    // gating decision the polling layer just made.
    let current_rollout_ids: std::collections::HashSet<String> = fleet
        .channels
        .keys()
        .filter_map(|ch| {
            nixfleet_reconciler::compute_rollout_id_for_channel(
                fleet,
                &snapshot.fleet_resolved_hash,
                ch,
            )
            .ok()
            .flatten()
        })
        .collect();
    let dispatch_snapshot: Vec<_> = dispatch_snapshot
        .into_iter()
        .filter(|r| !superseded.contains(&r.rollout_id))
        .filter(|r| current_rollout_ids.contains(&r.rollout_id))
        .collect();

    let observed = observed_projection::project(
        &checkins,
        &channel_refs,
        &dispatch_snapshot,
        HashMap::new(),
        HashMap::new(),
        &HashMap::new(),
    );

    // Augment with rollouts that exist in the rollouts table but have no
    // host_dispatch_state rows yet — newly-recorded rollouts the polling
    // layer just opened, where no agent has checked in to receive a
    // dispatch. predecessor_channel_blocking's "empty host_states ⇒
    // active for ordering" rule handles these correctly. Without this,
    // /v1/deferrals' view of "predecessor active" lags the polling
    // layer's view by up to one agent-checkin interval.
    let mut observed = observed;
    if let Some(db) = state.db.as_deref() {
        if let Ok(table_rollouts) = db.rollouts().list_active() {
            let known: std::collections::HashSet<String> = observed
                .active_rollouts
                .iter()
                .map(|r| r.id.clone())
                .collect();
            for r in table_rollouts {
                if known.contains(&r.rollout_id)
                    || superseded.contains(&r.rollout_id)
                    || !current_rollout_ids.contains(&r.rollout_id)
                {
                    continue;
                }
                let target_ref = channel_refs.get(&r.channel).cloned().unwrap_or_default();
                observed
                    .active_rollouts
                    .push(nixfleet_reconciler::observed::Rollout {
                        id: r.rollout_id,
                        channel: r.channel,
                        target_ref,
                        state: nixfleet_reconciler::RolloutState::Executing,
                        current_wave: r.current_wave as usize,
                        host_states: HashMap::new(),
                        last_healthy_since: HashMap::new(),
                        budgets: vec![],
                    });
            }
        }
    }

    let mut deferrals: Vec<serde_json::Value> = Vec::new();
    for (channel, current_ref) in &channel_refs {
        if !fleet.channels.contains_key(channel) {
            continue;
        }
        // Only channels with a ref change still pending an OpenRollout
        // can be in a "deferred" state. If the channel already has an
        // active rollout for this ref, it's not deferred — it's running.
        let has_active = observed
            .active_rollouts
            .iter()
            .any(|r| &r.channel == channel);
        if has_active {
            continue;
        }
        // Empty in-tick set: this is a live snapshot read, not a reconcile
        // tick. Domain truth is "what's blocked given the persisted active
        // rollouts"; in-tick OpenRollouts are not yet authoritative until
        // the next reconcile tick records them.
        let no_in_tick_opens = std::collections::HashSet::new();
        if let Some(blocker) = nixfleet_reconciler::predecessor_channel_blocking(
            fleet,
            &observed,
            &no_in_tick_opens,
            channel,
        ) {
            let reason = fleet
                .channel_edges
                .iter()
                .find(|e| e.after == *channel && e.before == blocker)
                .and_then(|e| e.reason.clone())
                .unwrap_or_else(|| {
                    format!("predecessor channel '{blocker}' has an unfinished rollout")
                });
            deferrals.push(serde_json::json!({
                "channel": channel,
                "targetRef": current_ref,
                "blockedBy": blocker,
                "reason": reason,
            }));
        }
    }

    // Stable order: alphabetical by channel name. The reconciler doesn't
    // care, but the dashboard panel benefits from determinism across ticks.
    deferrals.sort_by(|a, b| {
        a.get("channel")
            .and_then(|v| v.as_str())
            .cmp(&b.get("channel").and_then(|v| v.as_str()))
    });

    let body = serde_json::json!({ "deferrals": deferrals }).to_string();
    Ok(json_response(body))
}

fn empty_body() -> (HeaderMap, String) {
    json_response(r#"{"deferrals":[]}"#.to_string())
}

fn json_response(body: String) -> (HeaderMap, String) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (headers, body)
}
