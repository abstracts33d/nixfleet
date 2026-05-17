//! `POST /v1/agent/heartbeat` — agent liveness + drift detection
//! (RFC-0008 §4.3). Replaces the v0.1 `POST /v1/agent/checkin` flow.
//!
//! The agent posts a minimal envelope (hostname, optional rollout_id,
//! optional current_closure). CP:
//!
//! 1. Authenticates via mTLS (existing `require_cn_layer`).
//! 2. Checks cert CN's machine_id against body hostname (FORBIDDEN on
//!    mismatch — same shape as /v1/agent/events).
//! 3. Forwards a `HeartbeatReceived` input to the reducer with a oneshot
//!    reply.
//! 4. The reducer updates `last_heartbeat_at` (in-memory) and compares
//!    the agent's `current_closure` against the CP-mirror's
//!    `current_closure` field on the matching host_rollout_records row.
//!    Mismatch → reply contains `last_event_seq` for Replay-From.
//! 5. Response is 200 with optional `X-Nixfleet-Replay-From: <seq>`
//!    header.
//!
//! The 5-second reducer-reply timeout is generous: a healthy reducer
//! processes a heartbeat input in microseconds. A timeout here signals
//! reducer wedge (stuck applier? deadlock?) — log at error, return 503,
//! agent retries.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::super::middleware::AuthenticatedCn;
use super::super::state::AppState;
use crate::runtime::{HeartbeatReply, ReducerInput};

const REDUCER_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatRequest {
    pub hostname: String,
    /// Agent's view of the active rollout. `None` when the agent has no
    /// outstanding rollout (post-Converged steady state). Serde-
    /// transparent: the wire JSON shape is unchanged from the prior
    /// `String` typing (RFC-0012 §6.3 + D-007).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<nixfleet_proto::RolloutId>,
    /// Agent's observed `current` closure hash (what the host is
    /// actually running right now). CP cross-checks against its mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_closure: Option<String>,
    /// Caller-supplied timestamp (from agent's ClockHandle). CP uses
    /// this rather than wallclock-now so log lines line up with
    /// agent-side timing.
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatResponse {
    /// Mirrors `last_heartbeat_at` after the update. Useful for the
    /// agent to confirm CP saw it; null if the reducer was unable to
    /// process the heartbeat for some reason.
    pub received_at: Option<DateTime<Utc>>,
}

pub(in crate::server) async fn heartbeat(
    State(state): State<Arc<AppState>>,
    Extension(cn): Extension<AuthenticatedCn>,
    Json(req): Json<HeartbeatRequest>,
) -> Result<(HeaderMap, Json<HeartbeatResponse>), StatusCode> {
    let cn_str = cn.into_string();
    let machine_id = crate::auth::issuance::extract_machine_id(&cn_str, &state.agent_cn_suffix);
    if machine_id != req.hostname {
        tracing::warn!(
            target: "heartbeat",
            cert_cn = %cn_str,
            machine_id = %machine_id,
            body_hostname = %req.hostname,
            "heartbeat rejected: cert CN does not match body hostname",
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let Some(tx) = state.runtime_input_tx.get() else {
        tracing::warn!(
            target: "heartbeat",
            hostname = %req.hostname,
            "heartbeat: runtime not yet spun up; returning 503",
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    let input = ReducerInput::HeartbeatReceived {
        host: req.hostname.clone(),
        rollout_id: req.rollout_id.clone(),
        current_closure: req.current_closure.clone(),
        at: req.at,
        reply: reply_tx,
    };
    if let Err(err) = tx.try_send(input) {
        match err {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                tracing::warn!(
                    target: "heartbeat",
                    hostname = %req.hostname,
                    "heartbeat: reducer channel full; returning 503",
                );
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                tracing::error!(
                    target: "heartbeat",
                    hostname = %req.hostname,
                    "heartbeat: reducer channel closed; CP shutting down",
                );
            }
        }
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    // Wait for the reducer's drift verdict. Short timeout because a
    // healthy reducer answers in microseconds; a multi-second wedge is
    // a runtime bug.
    let reply = match tokio::time::timeout(REDUCER_REPLY_TIMEOUT, reply_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_recv_err)) => {
            tracing::error!(
                target: "heartbeat",
                hostname = %req.hostname,
                "heartbeat: reducer dropped reply oneshot without responding",
            );
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Err(_elapsed) => {
            tracing::error!(
                target: "heartbeat",
                hostname = %req.hostname,
                timeout_secs = REDUCER_REPLY_TIMEOUT.as_secs(),
                "heartbeat: reducer did not respond within timeout (possible wedge)",
            );
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    tracing::info!(
        target: "heartbeat",
        hostname = %req.hostname,
        rollout_id = ?req.rollout_id,
        current_closure = ?req.current_closure,
        replay_from = ?reply.replay_from,
        "heartbeat received",
    );

    let mut headers = HeaderMap::new();
    if let Some(seq) = reply.replay_from {
        // The agent's MUST-honor header: on drift, it re-POSTs events
        // from `seq` onward to /v1/agent/events. The exact handshake is
        // spelled out in RFC-0008 §4.3.
        headers.insert(
            "X-Nixfleet-Replay-From",
            HeaderValue::from_str(&seq.to_string()).expect("u64 to_string is always ASCII"),
        );
    }
    Ok((
        headers,
        Json(HeartbeatResponse {
            received_at: Some(req.at),
        }),
    ))
}
