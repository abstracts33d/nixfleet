//! Read-only status endpoints and closure proxy fallback.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::Utc;
use nixfleet_proto::{HostRolloutState, HostStatusEntry, HostsResponse};
use nixfleet_state_machine::HostState;
use serde::Serialize;

use super::super::middleware::AuthenticatedCn;
use super::super::state::AppState;

/// Map the reducer's internal `HostState` to the wire-side `HostRolloutState`.
/// 1:1 — proto's wire variants match the state-machine's enum exactly.
fn host_state_to_wire(s: HostState) -> HostRolloutState {
    match s {
        HostState::Pending => HostRolloutState::Pending,
        HostState::Activating => HostRolloutState::Activating,
        HostState::Deferred => HostRolloutState::Deferred,
        HostState::Soaking => HostRolloutState::Soaking,
        HostState::Converged => HostRolloutState::Converged,
        HostState::Failed => HostRolloutState::Failed,
        HostState::Reverted => HostRolloutState::Reverted,
    }
}

#[derive(Debug, Serialize)]
pub(in crate::server) struct WhoamiResponse {
    cn: String,
    /// RFC3339; moment we observed the verified identity, not the cert's notBefore.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` - verified mTLS CN of the caller.
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

/// `GET /v1/channels/{name}` - 503 until verified snapshot primed; 404 if channel undeclared.
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

/// `GET /v1/hosts` - per-host status overview, projected from
/// `host_rollout_records` (RFC-0005 §5).
///
/// One entry per (rollout, host) pair across all non-superseded rollouts.
/// Fields the v0.1 schema carried but the new schema doesn't (per-host
/// uptime, pending_reboot, quarantined_closure, last_checkin_at) stay at
/// their defaults — the agent runtime rewrite (Plan 07 / Phase 7-agent)
/// is where those re-attach to wire reports.
pub(in crate::server) async fn hosts_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<HostsResponse>, StatusCode> {
    let Some(db) = state.db.as_ref() else {
        return Ok(Json(HostsResponse { hosts: Vec::new() }));
    };

    // Enforce-mode probe-failure counts, keyed by (rollout, host).
    // Source: probe_failures projection (RFC-0007 §7.2). **Phase 9a**:
    // unwritten until 9b — values flow once the applier co-write lands.
    let outstanding = db
        .probe_failures()
        .outstanding_failing_enforce_probes_by_rollout()
        .unwrap_or_default();

    let mut hosts: Vec<HostStatusEntry> = Vec::new();
    let rollouts = match db.rollouts().list_active() {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(target: "hosts_status", error = %err, "rollouts.list_active failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Declared closure per host: best-effort lookup from the verified
    // fleet snapshot. None when verified_fleet isn't primed (early boot).
    let verified = state.verified_fleet.read().await.clone();

    for r in rollouts.iter() {
        let records = match db
            .host_rollout_records()
            .all_for_rollout(r.rollout_id.as_str())
        {
            Ok(rs) => rs,
            Err(err) => {
                tracing::error!(
                    target: "hosts_status",
                    rollout_id = %r.rollout_id,
                    error = %err,
                    "all_for_rollout failed; skipping rollout",
                );
                continue;
            }
        };
        for row in records {
            let declared_closure_hash = verified
                .as_ref()
                .and_then(|s| s.fleet.hosts.get(&row.hostname))
                .and_then(|h| h.closure_hash.clone());
            let compliance_count = outstanding
                .get(&r.rollout_id)
                .and_then(|per_host| per_host.get(&row.hostname))
                .copied()
                .unwrap_or(0);
            hosts.push(HostStatusEntry {
                hostname: row.hostname.clone(),
                channel: row.channel.clone(),
                declared_closure_hash,
                current_closure_hash: row.current_closure.clone(),
                pending_closure_hash: row.current_closure_at_dispatch.clone(),
                last_checkin_at: None,
                last_rollout_id: Some(row.rollout_id.as_str().to_string()),
                converged: row.state == HostState::Converged,
                outstanding_compliance_failures: compliance_count,
                outstanding_runtime_gate_errors: 0,
                verified_event_count: row.last_event_seq as usize,
                last_uptime_secs: None,
                rollout_state: Some(host_state_to_wire(row.state)),
                pending_reboot: false,
                quarantined_closure: None,
                pin: verified
                    .as_ref()
                    .and_then(|s| s.fleet.hosts.get(&row.hostname))
                    .and_then(|h| h.pin.clone()),
                outstanding_health_failures: 0,
            });
        }
    }
    Ok(Json(HostsResponse { hosts }))
}

/// `GET /v1/agent/closure/{hash}` - narinfo proxy fallback; 501 when no upstream configured.
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

#[cfg(test)]
mod tests {
    //! Happy-path projection tests for `/v1/hosts`. The route reads from
    //! host_rollout_records + the rollouts table; we set up both directly
    //! and call the handler in-process (no router/middleware).

    use super::*;
    use crate::db::Db;
    use nixfleet_state_machine::{HostRolloutState, HostState};

    fn fresh_state() -> Arc<AppState> {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        Arc::new(AppState {
            db: Some(Arc::new(db)),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn hosts_status_empty_with_no_rollouts() {
        let state = fresh_state();
        let resp = hosts_status(State(state)).await.unwrap();
        assert!(resp.0.hosts.is_empty(), "no rollouts ⇒ empty hosts list");
    }

    #[tokio::test]
    async fn hosts_status_projects_host_rollout_records() {
        let state = fresh_state();
        let db = state.db.clone().unwrap();
        let rollout = "stable@abc12345";
        let channel = "stable";
        db.rollouts()
            .record_rollout_opened(rollout, channel, rollout, chrono::Utc::now(), None)
            .unwrap();

        let now = chrono::Utc::now();
        let mut row = HostRolloutState::new_pending(
            rollout.into(),
            "h1".to_string(),
            channel.to_string(),
            "h1-closure".to_string(),
            now,
            now + chrono::Duration::minutes(5),
        );
        row.state = HostState::Converged;
        row.current_closure = Some("h1-closure".into());
        row.converged_at = Some(now + chrono::Duration::minutes(10));
        row.last_event_seq = 7;
        db.host_rollout_records().upsert(&row).unwrap();

        let resp = hosts_status(State(state.clone())).await.unwrap();
        assert_eq!(resp.0.hosts.len(), 1);
        let h = &resp.0.hosts[0];
        assert_eq!(h.hostname, "h1");
        assert_eq!(h.channel, "stable");
        assert_eq!(h.last_rollout_id.as_deref(), Some(rollout));
        assert!(h.converged, "state == Converged ⇒ converged = true");
        assert_eq!(
            h.rollout_state,
            Some(nixfleet_proto::HostRolloutState::Converged),
        );
        assert_eq!(h.current_closure_hash.as_deref(), Some("h1-closure"));
        assert_eq!(h.verified_event_count, 7);
    }
}
