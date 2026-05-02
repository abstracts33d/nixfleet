//! Read-only status endpoints: `/v1/whoami`, `/v1/channels/{name}`,
//! `/v1/hosts`, and the `/v1/agent/closure/{hash}` proxy fallback.

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
    /// rfc3339-formatted timestamp the server received the request.
    /// `issuedAt` semantically refers to "the moment we observed
    /// this verified identity", not the cert's notBefore.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` — returns the verified mTLS CN of the caller.
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
    /// Channel name as declared in `fleet.resolved.channels`.
    name: String,
    /// CI commit currently on the verified `fleet.resolved`
    /// snapshot — the ref the CP is rolling out toward. Mirrors
    /// `Observed.channel_refs[name]` once the projection has
    /// caught up. `None` when the verified snapshot's
    /// `meta.ciCommit` is unset (offline / file-backed deploys).
    declared_ci_commit: Option<String>,
    /// rfc3339 of the verified `meta.signedAt`. Operators read
    /// this to confirm the snapshot is fresh.
    signed_at: Option<String>,
    /// Channel's per-policy `freshnessWindow` (minutes), so
    /// operators can compare `now - signedAt` against the gate
    /// without re-fetching the artifact.
    freshness_window_minutes: u32,
}

/// `GET /v1/channels/{name}` — declared vs currently-rolled
/// snapshot for a channel ( acceptance criterion). Reads
/// from the in-memory verified-fleet snapshot — the same source
/// of truth dispatch decisions are made against. Returns 404 when
/// the channel is not declared in the verified `FleetResolved`.
/// Returns 503 when no verified snapshot has been primed yet
/// (CP just booted; agents will see 503 on this endpoint until
/// the channel-refs poll succeeds).
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
    /// Hostname per `fleet.resolved.hosts`.
    hostname: String,
    /// Channel the host is on (declarative).
    channel: String,
    /// Closure declared as the host's target on the most recent
    /// verified `fleet.resolved.json` snapshot. Null when the
    /// fleet hasn't been signed since CI started stamping
    /// closure hashes.
    declared_closure_hash: Option<String>,
    /// Closure the host is currently running, per its most recent
    /// checkin. `None` when the host has never checked in.
    current_closure_hash: Option<String>,
    /// Closure queued for next boot, if any. Null when current ==
    /// pending (the typical converged-and-rebooted state).
    pending_closure_hash: Option<String>,
    /// Wall-clock of the most recent checkin . `None`
    /// when the host has never checked in.
    last_checkin_at: Option<String>,
    /// Rollout id the host most recently confirmed against, per
    /// the agent's last `last_evaluated_target` echo. Useful for
    /// operators to see "host X is on rollout Y" without
    /// scraping the journal.
    last_rollout_id: Option<String>,
    /// True iff the host's current closure matches the declared
    /// closure on the verified fleet snapshot. Mirrors the
    /// dispatch decision `Decision::Converged`.
    converged: bool,
    /// Count of outstanding `ComplianceFailure` events in the
    /// host's report buffer (events whose rollout matches the
    /// host's current rollout — i.e. events not yet
    /// resolved-by-replacement). 0 when the host has no
    /// outstanding events.
    outstanding_compliance_failures: usize,
    /// Count of outstanding `RuntimeGateError` events (same
    /// resolution semantics as `outstanding_compliance_failures`).
    outstanding_runtime_gate_errors: usize,
    /// Count of `signature_status = Verified` events in the
    /// host's report buffer. Auditor-chain visibility metric
    /// when this matches `outstanding_compliance_failures +
    /// outstanding_runtime_gate_errors`, every outstanding
    /// failure has a verified signature against the host's
    /// `fleet.resolved.hosts.<n>.pubkey`.
    verified_event_count: usize,
}

/// `GET /v1/hosts` — fleet-wide observed state, per host.
///
/// Joins the verified fleet snapshot's host declarations with the
/// CP's per-host checkin record + report buffer. Replaces the
/// brittle journal-scraping path that fleet-status' render.sh
/// previously used (the JSON tracing subscriber emits structured
/// fields that don't match the bash awk parser's `key=value`
/// expectation).
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

            // Walk the host's report buffer, counting outstanding
            // ComplianceFailure / RuntimeGateError events scoped to
            // the host's current rollout id (resolution-by-
            // replacement: events for older rollouts are considered
            // resolved).
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
                    // Resolution-by-replacement check: skip events
                    // whose rollout the host has moved past.
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

/// `GET /v1/agent/closure/{hash}` — closure proxy fallback for hosts
/// that can't reach the binary cache directly. Forwards narinfo
/// requests to the configured cache upstream (any nix-cache-protocol
/// HTTP backend: harmonia, attic, cachix, plain nix-serve, …). Real
/// Nix-cache-protocol forwarding (full nar streaming) is a follow-up
/// PR; this lands the wire shape + the upstream config path.
///
/// When `closure_upstream` is unset, returns 501 Not Implemented.
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
