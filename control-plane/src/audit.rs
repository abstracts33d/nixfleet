use axum::extract::{Query, State};
use axum::Json;
use nixfleet_types::AuditEvent;
use serde::Deserialize;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub actor: Option<String>,
    pub action: Option<String>,
    pub target: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    100
}

/// GET /api/v1/audit
pub async fn list_audit_events(
    State((_state, db)): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> Json<Vec<AuditEvent>> {
    let events = db
        .query_audit_events(
            query.actor.as_deref(),
            query.action.as_deref(),
            query.target.as_deref(),
            query.limit,
        )
        .unwrap_or_default();
    Json(events)
}
