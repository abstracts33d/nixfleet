//! `/v1/agent/report` handler plus its signature-verification and
//! event-id helpers.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use nixfleet_proto::agent_wire::{ReportRequest, ReportResponse};

use crate::auth_cn::PeerCertificates;

use super::middleware::require_cn;
use super::state::{AppState, ReportRecord, REPORT_RING_CAP};

/// `POST /v1/agent/report` — record an out-of-band event report.
///
/// In-memory ring buffer per host, capped at `REPORT_RING_CAP`.
/// New reports push to the back; oldest is dropped on overflow.
/// Future work: promote to SQLite + correlate with rollouts.
pub(super) async fn report(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ReportRequest>,
) -> Result<Json<ReportResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "report rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let event_id = format!(
        "evt-{}-{}",
        Utc::now().timestamp_millis(),
        rand_suffix(8)
    );
    let received_at = Utc::now();

    // Render the event variant for the journal in a grep-friendly
    // way: `event=activation-failed`, `event=verify-mismatch`, etc.
    // The serde_json round-trip extracts the kebab-case discriminator.
    let event_str = serde_json::to_value(&req.event)
        .ok()
        .and_then(|v| v.get("event").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string());
    let rollout_str = req
        .rollout
        .clone()
        .unwrap_or_else(|| "<none>".to_string());

    // root-3 / #59 — verify probe-output signatures on the
    // two event variants that carry them. Non-signed events surface
    // as `None`; the wave-staging gate consults `signature_status`
    // when honouring outstanding events. Verification is best-
    // effort: we always store the record (mTLS already authenticated
    // the post), the signature verdict shapes downstream gating.
    let signature_status = compute_signature_status(&state, &req).await;

    tracing::info!(
        target: "report",
        hostname = %req.hostname,
        event = %event_str,
        rollout = %rollout_str,
        agent_version = %req.agent_version,
        event_id = %event_id,
        signature_status = ?signature_status,
        "report received"
    );

    let record = ReportRecord {
        event_id: event_id.clone(),
        received_at,
        report: req.clone(),
        signature_status,
    };

    // — write through to SQLite alongside the in-memory
    // ring buffer. Ring stays for hot-path latency in dispatch
    // decisions; SQLite is the durable record that survives CP
    // restart. DB write is best-effort: a failure logs warn + lets
    // the in-memory write proceed (degraded == old in-memory-only
    // behaviour, so no regression).
    if let Some(db) = state.db.as_ref() {
        let signature_status_str = signature_status.as_ref().and_then(|s| {
            serde_json::to_value(s).ok().and_then(|v| {
                v.as_str().map(String::from)
            })
        });
        // Best-effort SQLite persistence. Two failure modes, both
        // handled the same way: log + skip the DB write, let the
        // in-memory ring buffer below absorb the event regardless.
        // The serde failure path is what matters here — previously
        // `unwrap_or_default ` would write `""` into report_json,
        // and on next CP startup the hydration parse would fail and
        // skip the row, leaving a phantom DB row that could never
        // be replayed. Now we never write the row at all on serde
        // failure.
        match serde_json::to_string(&req) {
            Ok(report_json) => {
                if let Err(err) = db.record_host_report(&crate::db::HostReportInsert {
                    hostname: &req.hostname,
                    event_id: &event_id,
                    received_at,
                    event_kind: &event_str,
                    rollout: req.rollout.as_deref(),
                    signature_status: signature_status_str.as_deref(),
                    report_json: &report_json,
                }) {
                    tracing::warn!(
                        target: "report",
                        hostname = %req.hostname,
                        event_id = %event_id,
                        error = %err,
                        "report SQLite write failed; in-memory ring buffer still updated",
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    target: "report",
                    hostname = %req.hostname,
                    event_id = %event_id,
                    error = %err,
                    "report serialisation to JSON failed; skipping SQLite persistence (in-memory ring still updated)",
                );
            }
        }
    }

    let mut reports = state.host_reports.write().await;
    let buf = reports.entry(req.hostname).or_default();
    if buf.len() >= REPORT_RING_CAP {
        buf.pop_front();
    }
    buf.push_back(record);

    Ok(Json(ReportResponse { event_id }))
}

/// Compute the signature verdict for an incoming report (
/// root-3 / #59). Only `ComplianceFailure` and `RuntimeGateError`
/// carry probe-output signatures today; all other variants return
/// `None`. The host's pubkey comes from `verified_fleet`'s
/// `hosts.<hostname>.pubkey`; absent pubkey → `NoPubkey`.
async fn compute_signature_status(
    state: &Arc<AppState>,
    req: &ReportRequest,
) -> Option<nixfleet_reconciler::evidence::SignatureStatus> {
    use nixfleet_proto::agent_wire::ReportEvent;
    use nixfleet_proto::evidence_signing::{
        ComplianceFailureSignedPayload, RuntimeGateErrorSignedPayload,
    };
    use nixfleet_reconciler::evidence::verify_event;

    let pubkey: Option<String> = {
        let fleet_guard = state.verified_fleet.read().await;
        fleet_guard
            .as_ref()
            .and_then(|f| f.hosts.get(&req.hostname))
            .and_then(|h| h.pubkey.clone())
    };

    match &req.event {
        ReportEvent::ComplianceFailure {
            control_id,
            status,
            framework_articles,
            evidence_snippet,
            evidence_collected_at,
            signature,
        } => {
            // Re-derive the snippet hash the agent included in its
            // signed payload (sha256 of JCS-canonical snippet bytes;
            // empty when snippet is None).
            let snippet_sha = match evidence_snippet {
                Some(v) => match serde_jcs::to_vec(v) {
                    Ok(bytes) => {
                        use sha2::Digest;
                        let d = sha2::Sha256::digest(&bytes);
                        let mut s = String::with_capacity(64);
                        for b in d.iter() {
                            s.push_str(&format!("{:02x}", b));
                        }
                        s
                    }
                    Err(_) => String::new(),
                },
                None => String::new(),
            };
            let payload = ComplianceFailureSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                control_id,
                status,
                framework_articles,
                evidence_collected_at: *evidence_collected_at,
                evidence_snippet_sha256: snippet_sha,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::RuntimeGateError {
            reason,
            collector_exit_code,
            evidence_collected_at,
            activation_completed_at,
            signature,
        } => {
            let payload = RuntimeGateErrorSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                reason,
                collector_exit_code: *collector_exit_code,
                evidence_collected_at: *evidence_collected_at,
                activation_completed_at: *activation_completed_at,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        _ => None,
    }
}

/// 8-char lowercase-alnum suffix for event IDs. Not crypto-grade
/// just enough to make IDs visually distinct in journal output.
fn rand_suffix(n: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}
