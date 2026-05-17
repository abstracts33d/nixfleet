//! `GET /v1/deferrals` - currently-blocked (rollout, host) pairs as observed
//! by the latest gate decisions in `event_log`.
//!
//! Each row in `event_log` with `kind = 'gate_decision'` represents one
//! `plan_next` pass blocking a host's dispatch (Phase 5b's
//! `PlanAction::DeferDispatch`). We dedupe to one entry per (host, rollout)
//! pair keeping the most recent decision; that gives operators "what's
//! holding things up right now" without the historical stream — full
//! history stays available via the raw event_log query.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;

use super::super::state::AppState;
use crate::db::event_log::EventLogKind;

/// Max rows scanned from event_log. v0.2 scale (~256 hosts × ~few gate
/// decisions per dispatch attempt) keeps practical totals well under this
/// even mid-rollout. If you raise this, also bump it in the CLI.
const SCAN_LIMIT: i64 = 1024;

pub(in crate::server) async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let payload = project_deferrals(&state)?;
    let body = payload.to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((headers, body))
}

/// Pure projection from `event_log` to the deferrals response shape.
/// Split out from the route handler so unit tests can assert against
/// `serde_json::Value` directly without exercising the axum response
/// pipeline.
fn project_deferrals(state: &Arc<AppState>) -> Result<serde_json::Value, StatusCode> {
    let Some(db) = state.db.as_ref() else {
        return Ok(serde_json::json!({ "deferrals": [] }));
    };
    let rows = match db
        .event_log()
        .query_by_kind(EventLogKind::GateDecision, SCAN_LIMIT)
    {
        Ok(rs) => rs,
        Err(err) => {
            tracing::error!(target: "deferrals", error = %err, "event_log query failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut deferrals: Vec<serde_json::Value> = Vec::new();
    for row in rows.into_iter().rev() {
        let payload: serde_json::Value = match serde_json::from_str(&row.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let host = payload
            .get("host")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let rollout = payload
            .get("rollout")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if host.is_empty() || rollout.is_empty() {
            continue;
        }
        if !seen.insert((host.to_string(), rollout.to_string())) {
            continue;
        }
        let gate = payload
            .get("gate")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let reason = payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        deferrals.push(serde_json::json!({
            "host": host,
            "rollout": rollout,
            "blockedBy": gate,
            "reason": reason,
            "observedAt": row.ts.to_rfc3339(),
        }));
    }
    Ok(serde_json::json!({ "deferrals": deferrals }))
}

#[cfg(test)]
mod tests {
    //! Pure-projection tests against `project_deferrals` — the route
    //! handler is a thin axum wrapper around it.

    use super::*;
    use crate::db::Db;
    use crate::db::event_log::EventLogEntry;
    use chrono::Utc;

    fn fresh_state() -> Arc<AppState> {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        Arc::new(AppState {
            db: Some(Arc::new(db)),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn deferrals_empty_when_no_gate_decisions() {
        let state = fresh_state();
        let v = project_deferrals(&state).unwrap();
        assert_eq!(v["deferrals"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn deferrals_dedups_to_latest_per_host_rollout_pair() {
        let state = fresh_state();
        let db = state.db.clone().unwrap();
        let now = Utc::now();
        // Two decisions for (h2, stable): older says wave-promotion,
        // newer says compliance-wave. Expect the latest only.
        db.event_log()
            .append(&EventLogEntry {
                kind: EventLogKind::GateDecision,
                ts: now,
                host_id: Some("h2".into()),
                rollout_id: Some("stable".into()),
                payload:
                    r#"{"host":"h2","rollout":"stable","gate":"wave-promotion","reason":"earlier"}"#
                        .into(),
            })
            .unwrap();
        db.event_log()
            .append(&EventLogEntry {
                kind: EventLogKind::GateDecision,
                ts: now + chrono::Duration::seconds(1),
                host_id: Some("h2".into()),
                rollout_id: Some("stable".into()),
                payload:
                    r#"{"host":"h2","rollout":"stable","gate":"compliance-wave","reason":"latest"}"#
                        .into(),
            })
            .unwrap();

        let v = project_deferrals(&state).unwrap();
        let arr = v["deferrals"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "dedup keeps one entry per (host, rollout)");
        assert_eq!(arr[0]["host"], "h2");
        assert_eq!(arr[0]["blockedBy"], "compliance-wave");
        assert_eq!(arr[0]["reason"], "latest");
    }
}
