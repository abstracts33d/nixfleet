//! Read-only status endpoints and closure proxy fallback.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use serde::Serialize;

use super::super::middleware::AuthenticatedCn;
use super::super::state::AppState;

#[derive(Debug, Serialize)]
pub(in crate::server) struct WhoamiResponse {
    cn: String,
    /// RFC3339; moment we observed the verified identity, not the cert's notBefore.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` — verified mTLS CN of the caller.
pub(in crate::server) async fn whoami(
    Extension(cn): Extension<AuthenticatedCn>,
) -> Json<WhoamiResponse> {
    Json(WhoamiResponse {
        cn: cn.into_string(),
        issued_at: Utc::now().to_rfc3339(),
    })
}

#[derive(Debug, Serialize)]
pub(in crate::server) struct ChannelStatusResponse {
    name: String,
    /// `None` when offline / file-backed deploys leave `meta.ciCommit` unset.
    declared_ci_commit: Option<String>,
    signed_at: Option<String>,
    freshness_window_minutes: u32,
}

/// `GET /v1/channels/{name}` — 503 until verified snapshot primed; 404 if channel undeclared.
pub(in crate::server) async fn channel_status(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<ChannelStatusResponse>, StatusCode> {
    let snapshot = state.verified_fleet.read().await.clone();
    let snap = snapshot.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let fleet = snap.fleet;
    let channel = fleet.channels.get(&name).ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ChannelStatusResponse {
        name,
        declared_ci_commit: fleet.meta.ci_commit.clone(),
        signed_at: fleet.meta.signed_at.map(|t| t.to_rfc3339()),
        freshness_window_minutes: channel.freshness_window,
    }))
}

#[derive(Debug, Serialize)]
pub(in crate::server) struct HostsResponse {
    hosts: Vec<HostStatusEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::server) struct HostStatusEntry {
    hostname: String,
    channel: String,
    declared_closure_hash: Option<String>,
    current_closure_hash: Option<String>,
    pending_closure_hash: Option<String>,
    last_checkin_at: Option<String>,
    last_rollout_id: Option<String>,
    converged: bool,
    outstanding_compliance_failures: usize,
    outstanding_runtime_gate_errors: usize,
    verified_event_count: usize,
}

/// `GET /v1/hosts` — joins verified fleet declarations with per-host checkins and report buffers.
pub(in crate::server) async fn hosts_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<HostsResponse>, StatusCode> {
    let fleet = state
        .verified_fleet
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .fleet;
    let checkins = state.host_checkins.read().await;
    let reports = state.host_reports.read().await;

    let mut entries: Vec<HostStatusEntry> = fleet
        .hosts
        .iter()
        .map(|(hostname, host_decl)| {
            let checkin = checkins.get(hostname);
            let last_checkin_at = checkin.map(|c| c.last_checkin.to_rfc3339());
            let current = checkin.map(|c| c.checkin.current_generation.closure_hash.clone());
            let pending = checkin.and_then(|c| {
                c.checkin
                    .pending_generation
                    .as_ref()
                    .map(|p| p.closure_hash.clone())
            });
            let last_rollout_id = checkin.and_then(|c| {
                c.checkin
                    .last_evaluated_target
                    .as_ref()
                    .and_then(|t| t.rollout_id.clone())
            });
            let converged = match (&host_decl.closure_hash, &current) {
                (Some(declared), Some(running)) => declared == running,
                _ => false,
            };

            // GOTCHA: resolution-by-replacement — events from older rollouts are considered resolved.
            let host_buf = reports.get(hostname);
            let cur_rollout = last_rollout_id.as_deref();
            let mut compliance_failures = 0usize;
            let mut runtime_gate_errors = 0usize;
            let mut verified_count = 0usize;
            if let Some(buf) = host_buf {
                use nixfleet_proto::agent_wire::ReportEvent;
                for record in buf.iter() {
                    let is_compliance =
                        matches!(record.report.event, ReportEvent::ComplianceFailure { .. });
                    let is_runtime_gate =
                        matches!(record.report.event, ReportEvent::RuntimeGateError { .. });
                    if !is_compliance && !is_runtime_gate {
                        continue;
                    }
                    let event_rollout = record.report.rollout.as_deref();
                    let outstanding = !matches!(
                        (cur_rollout, event_rollout),
                        (Some(cur), Some(ev_r)) if cur != ev_r
                    );
                    if !outstanding {
                        continue;
                    }
                    if is_compliance {
                        compliance_failures += 1;
                    }
                    if is_runtime_gate {
                        runtime_gate_errors += 1;
                    }
                    if matches!(
                        record.signature_status,
                        Some(nixfleet_reconciler::evidence::SignatureStatus::Verified)
                    ) {
                        verified_count += 1;
                    }
                }
            }

            HostStatusEntry {
                hostname: hostname.clone(),
                channel: host_decl.channel.clone(),
                declared_closure_hash: host_decl.closure_hash.clone(),
                current_closure_hash: current,
                pending_closure_hash: pending,
                last_checkin_at,
                last_rollout_id,
                converged,
                outstanding_compliance_failures: compliance_failures,
                outstanding_runtime_gate_errors: runtime_gate_errors,
                verified_event_count: verified_count,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    Ok(Json(HostsResponse { hosts: entries }))
}

/// `GET /v1/agent/closure/{hash}` — narinfo proxy fallback; 501 when no upstream configured.
pub(in crate::server) async fn closure_proxy(
    State(state): State<Arc<AppState>>,
    Extension(cn): Extension<AuthenticatedCn>,
    Path(closure_hash): Path<String>,
) -> Result<Response, StatusCode> {
    let cn = cn.as_str();

    let upstream = match &state.closure_upstream {
        Some(u) => u,
        None => {
            tracing::info!(
                target: "closure_proxy",
                cn = %cn,
                closure = %closure_hash,
                "closure proxy hit but no --closure-upstream configured (501)"
            );
            let body = serde_json::json!({
                "error": "closure proxy not configured",
                "closure": closure_hash,
                "tracking": "set services.nixfleet-control-plane.closureUpstream",
            });
            return Ok(Response::builder()
                .status(StatusCode::NOT_IMPLEMENTED)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("Response::builder with valid status + body is infallible"));
        }
    };

    let url = format!(
        "{}/{}.narinfo",
        upstream.base_url.trim_end_matches('/'),
        closure_hash
    );
    tracing::debug!(target: "closure_proxy", cn = %cn, url = %url, "forwarding");

    let resp = match upstream.client.get(&url).send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(error = %err, "closure proxy: upstream unreachable");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("upstream error: {err}")))
                .expect("Response::builder with valid status + body is infallible"));
        }
    };
    let status = resp.status().as_u16();
    let body = resp.bytes().await.map_err(|err| {
        tracing::warn!(error = %err, "closure proxy: upstream body read failed");
        StatusCode::BAD_GATEWAY
    })?;
    Ok(Response::builder()
        .status(status)
        .header("content-type", "text/x-nix-narinfo")
        .body(Body::from(body))
        .expect("Response::builder with valid status + body is infallible"))
}
