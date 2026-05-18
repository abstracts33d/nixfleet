//! `POST /v1/agent/events` — inbound event ingestion (RFC-0005 §4.2).
//!
//! The agent posts a single `AgentEvent` per call. The handler:
//!
//! 1. Authenticates the caller via mTLS (`require_cn_layer` middleware
//!    has already verified the cert and stamped `AuthenticatedCn`).
//! 2. Cross-checks the cert CN's machine_id against the body's
//!    `hostname` — same pattern as `/v1/agent/report`. CN-vs-body
//!    mismatch ⇒ 403.
//! 3. Deduplicates by `(hostname, rollout_id, seq)` against the
//!    `host_rollout_records.last_event_seq` column. A seq ≤ the stored
//!    value is a replay/duplicate and silently 204s (the agent retries
//!    are idempotent by design).
//! 4. Maps the wire `AgentEvent` onto the matching
//!    `nixfleet_state_machine::Event::Remote*` variant and sends it into
//!    the reducer's input MPSC.
//! 5. Returns 204 on success, 503 if the runtime channel is unavailable
//!    (only observable during a narrow startup window before
//!    `serve()` wires `state.runtime_input_tx`).
//!
//! Signature verification on the body is a forward-looking TODO. v0.2
//! trusts the mTLS cert chain (RFC-0002 §3) — a Phase 7+ pass adds
//! per-event signatures so an event_log replay can detect tampering
//! against a stored cert change. The wire envelope already carries an
//! optional `signature` field so adding enforcement is non-breaking.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use nixfleet_proto::AgentEventEnvelope;
use nixfleet_state_machine::Event;

use super::super::middleware::AuthenticatedCn;
use super::super::state::AppState;
use crate::runtime::ReducerInput;

// Wire envelope + AgentEvent + the supporting Wire enums + the
// AgentEvent -> Event projection all live in `nixfleet_proto` and
// `nixfleet_state_machine` (RFC-0004 §2 lift: types crossing the
// agent <-> CP boundary live in a single canonical place). The
// duplicated definitions that lived here previously - and the
// hand-built JSON envelope on the agent side - both shipped a
// `rollout_id` vs `rolloutId` casing mismatch that this lift closes
// at the type level.

pub(in crate::server) async fn events(
    State(state): State<Arc<AppState>>,
    Extension(cn): Extension<AuthenticatedCn>,
    Json(envelope): Json<AgentEventEnvelope>,
) -> StatusCode {
    // 1. Cert-CN vs body-hostname guard. Same shape as the existing
    //    /v1/agent/report check (cf. routes/reports.rs).
    let cn_str = cn.into_string();
    let machine_id = crate::auth::issuance::extract_machine_id(&cn_str, &state.agent_cn_suffix);
    if machine_id != envelope.hostname {
        tracing::warn!(
            target: "events",
            cert_cn = %cn_str,
            machine_id = %machine_id,
            body_hostname = %envelope.hostname,
            "events rejected: cert CN does not match body hostname",
        );
        return StatusCode::FORBIDDEN;
    }

    let seq = envelope.event.seq();

    // 2. Dedup by (hostname, rollout_id, seq). Replay is silent 204.
    //    Source of truth: host_rollout_records.last_event_seq, advanced
    //    by the reducer when it actually applies the event. A duplicate
    //    here means the agent retried before the reducer caught up — or
    //    we already processed and the agent missed the response. Either
    //    way: idempotent.
    if let Some(db) = state.db.as_ref() {
        match db
            .host_rollout_records()
            .load(envelope.rollout_id.as_str(), &envelope.hostname)
        {
            Ok(Some(record)) if seq <= record.last_event_seq => {
                tracing::debug!(
                    target: "events",
                    hostname = %envelope.hostname,
                    rollout_id = %envelope.rollout_id,
                    seq,
                    last_event_seq = record.last_event_seq,
                    "events: duplicate seq, 204 idempotent",
                );
                return StatusCode::NO_CONTENT;
            }
            Ok(_) => {} // first event for this pair, or new seq — fall through
            Err(err) => {
                tracing::error!(
                    target: "events",
                    hostname = %envelope.hostname,
                    rollout_id = %envelope.rollout_id,
                    error = %err,
                    "events: dedup lookup failed; dropping into reducer anyway",
                );
                // Continue — better to double-process than to silently drop.
                // The reducer's state-machine itself rejects illegal
                // transitions (e.g. duplicate DispatchAck on a Soaking host).
            }
        }
    }

    // 3. Push into reducer. 503 if runtime not yet spun up (narrow
    //    startup window only).
    let Some(tx) = state.runtime_input_tx.get() else {
        tracing::warn!(
            target: "events",
            hostname = %envelope.hostname,
            "events: runtime not yet spun up; returning 503",
        );
        return StatusCode::SERVICE_UNAVAILABLE;
    };

    let host = envelope.hostname.clone();
    let rollout_id = envelope.rollout_id.clone();
    let reducer_event: Event = envelope.event.into();
    let input = ReducerInput::HostEvent {
        host,
        rollout_id,
        event: reducer_event,
    };

    // Bounded MPSC backpressure surfaces as 503; the pull-only agent
    // contract (RFC-0005 §2.1) means agents will retry, so the channel
    // never builds an unbounded queue when CP is overloaded.
    match tx.try_send(input) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                target: "events",
                hostname = %envelope.hostname,
                rollout_id = %envelope.rollout_id,
                seq,
                "events: reducer input channel full; returning 503 (agent retries)",
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!(
                target: "events",
                hostname = %envelope.hostname,
                "events: reducer input channel closed; CP shutting down",
            );
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
