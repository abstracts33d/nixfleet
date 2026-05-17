//! `GET /v1/agent/dispatch?wait=60` — long-poll dispatch delivery.
//!
//! Replaces the pre-v0.2 dispatch-on-checkin contract. The agent calls
//! this every time it wants new work (typically right after a Converged
//! or every minute on idle, per `nixfleet-agent` policy). The contract
//! is RFC-0003 §1 pull-only + RFC-0008 §2.1 + plan 06's locked-in
//! "long-poll, 60s wait window" decision.
//!
//! Implementation:
//!
//!   1. mTLS + CN-vs-?hostname=… check (cert CN authoritative).
//!   2. Peek `dispatch_queue` for the host. Row exists ⇒ atomic
//!      `take_for_host` + return. No row ⇒ park on the
//!      `state.dispatch_kick` watch channel for up to `wait` seconds
//!      (capped at 60). Wake on:
//!         - applier upsert (any host) — re-peek for this host;
//!         - timeout — return empty.
//!   3. Response shape:
//!         - 200 + Dispatch JSON ⇒ work to do, agent processes;
//!         - 204                ⇒ no work, agent re-polls.
//!
//! Backpressure: long-polls are cheap (one row peek per wake), and the
//! watch channel collapses bursts to one wake. The 60s cap is the
//! protocol-defined ceiling.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::super::middleware::AuthenticatedCn;
use super::super::state::AppState;

/// Long-poll wait window cap. Locked by plan 06.
const MAX_WAIT_SECS: u64 = 60;

#[derive(Debug, Clone, Deserialize)]
pub struct DispatchQuery {
    /// Long-poll wait in seconds. Clamped to [0, 60].
    #[serde(default)]
    pub wait: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchResponse {
    pub hostname: String,
    pub rollout_id: nixfleet_proto::RolloutId,
    pub target_closure: String,
    pub soak_due_at: DateTime<Utc>,
    pub enqueued_at: DateTime<Utc>,
}

pub(in crate::server) async fn dispatch(
    State(state): State<Arc<AppState>>,
    Extension(cn): Extension<AuthenticatedCn>,
    Query(q): Query<DispatchQuery>,
) -> Result<(StatusCode, Json<Option<DispatchResponse>>), StatusCode> {
    let cn_str = cn.into_string();
    let hostname = crate::auth::issuance::extract_machine_id(&cn_str, &state.agent_cn_suffix);

    let Some(db) = state.db.as_ref() else {
        tracing::warn!(
            target: "dispatch",
            %hostname,
            "dispatch: no DB attached; returning 204 (no queue exists in in-memory mode)",
        );
        return Ok((StatusCode::NO_CONTENT, Json(None)));
    };

    // Fast path: a row is already waiting.
    match db.dispatch_queue().take_for_host(&hostname) {
        Ok(Some(q)) => return Ok(deliver(q)),
        Ok(None) => {} // park below
        Err(err) => {
            tracing::error!(
                target: "dispatch",
                %hostname,
                error = %err,
                "dispatch: take_for_host failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let wait = Duration::from_secs(q.wait.unwrap_or(MAX_WAIT_SECS).min(MAX_WAIT_SECS));
    let deadline = tokio::time::Instant::now() + wait;
    let mut kick_rx = state.dispatch_kick.subscribe();

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok((StatusCode::NO_CONTENT, Json(None)));
        }
        let remaining = deadline - now;

        tokio::select! {
            // Watch yields on any new dispatch_queue upsert. We don't
            // care about the value — re-peek the table for THIS host
            // and decide.
            res = kick_rx.changed() => {
                if res.is_err() {
                    // Sender dropped — CP is shutting down.
                    return Err(StatusCode::SERVICE_UNAVAILABLE);
                }
                // Fall through and try take_for_host below.
            }
            _ = tokio::time::sleep(remaining) => {
                return Ok((StatusCode::NO_CONTENT, Json(None)));
            }
        }

        match db.dispatch_queue().take_for_host(&hostname) {
            Ok(Some(q)) => return Ok(deliver(q)),
            Ok(None) => continue, // false wake — re-park
            Err(err) => {
                tracing::error!(
                    target: "dispatch",
                    %hostname,
                    error = %err,
                    "dispatch: take_for_host failed after kick",
                );
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
}

fn deliver(
    q: crate::db::dispatch_queue::QueuedDispatch,
) -> (StatusCode, Json<Option<DispatchResponse>>) {
    let resp = DispatchResponse {
        hostname: q.hostname,
        rollout_id: q.rollout_id,
        target_closure: q.target_closure,
        soak_due_at: q.soak_due_at,
        enqueued_at: q.enqueued_at,
    };
    (StatusCode::OK, Json(Some(resp)))
}
