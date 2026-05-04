//! `/metrics` — Prometheus text format. mTLS-protected like the rest of
//! `/v1/*`; lab Prometheus scrapes with the same agent identity it
//! presents to `/v1/hosts` (see fleet's monitoring-prometheus.nix
//! `nixfleet-cp` job).

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::metrics::{install_recorder, record_fleet_metrics};
use crate::server::AppState;
use crate::state_view::StateViewError;

/// `GET /metrics` — refresh gauges from in-memory state, then render
/// the global Prometheus recorder. 503 until the verified fleet
/// snapshot is primed (matches `/v1/hosts` semantics).
pub(in crate::server) async fn metrics_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    record_fleet_metrics(&state).await.map_err(|e| match e {
        StateViewError::FleetNotPrimed => StatusCode::SERVICE_UNAVAILABLE,
    })?;
    let body = install_recorder().render();
    Ok((
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
        .into_response())
}
