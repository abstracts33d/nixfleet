//! `GET /healthz` — operator status probe.
//!
//! Lives outside the `/v1/*` namespace so it bypasses the
//! protocol-version middleware (status probes should always reply,
//! regardless of header version drift).

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use super::super::state::AppState;

#[derive(Debug, Serialize)]
pub(in crate::server) struct HealthzResponse {
    ok: bool,
    version: &'static str,
    /// rfc3339-formatted UTC timestamp, or `null` if the reconcile
    /// loop has not yet ticked once. (Realistic only for the first
    /// ~30s after boot.)
    last_tick_at: Option<String>,
}

pub(in crate::server) async fn healthz(state: State<Arc<AppState>>) -> Json<HealthzResponse> {
    let last = *state.last_tick_at.read().await;
    Json(HealthzResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        last_tick_at: last.map(|t| t.to_rfc3339()),
    })
}
