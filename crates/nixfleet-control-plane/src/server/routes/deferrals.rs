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
    let rollouts = match state
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
    // Filter superseded rollouts so a stale entry doesn't mark the
    // predecessor channel as still active. Mirrors the reconcile-loop's
    // pre-tick filtering.
    let rollouts = match state.db.as_deref().map(|db| db.rollouts().superseded_rollout_ids()) {
        Some(Ok(ids)) => {
            let dead: std::collections::HashSet<String> = ids.into_iter().collect();
            rollouts
                .into_iter()
                .filter(|r| !dead.contains(&r.rollout_id))
                .collect()
        }
        _ => rollouts,
    };

    // last_deferrals isn't consulted here on purpose — the panel needs
    // domain truth, not the debounce-map snapshot. The empty `last_deferrals`
    // we pass to project() reflects that.
    let observed = observed_projection::project(
        &checkins,
        &channel_refs,
        &rollouts,
        HashMap::new(),
        HashMap::new(),
    );

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
        if let Some(blocker) = nixfleet_reconciler::predecessor_channel_blocking(
            fleet,
            &observed,
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
