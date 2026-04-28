//! HTTP route handlers for the long-running CP server.
//!
//! Pulled out of the monolithic `server.rs`. Each handler is its
//! own free function with the route's signature; the router in
//! `serve.rs` (this module's parent) wires them under the `/v1/*`
//! tree. State + middleware are shared via the parent's `state` and
//! `middleware` modules.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest, ReportRequest, ReportResponse,
};
use nixfleet_proto::enroll_wire::{EnrollRequest, EnrollResponse, RenewRequest, RenewResponse};
use rcgen::PublicKeyData;
use serde::Serialize;

use crate::auth_cn::PeerCertificates;

use super::middleware::require_cn;
use super::state::{
    AppState, HostCheckinRecord, ReportRecord, CONFIRM_DEADLINE_SECS, NEXT_CHECKIN_SECS,
    REPORT_RING_CAP,
};

#[derive(Debug, Serialize)]
pub(super) struct HealthzResponse {
    ok: bool,
    version: &'static str,
    /// rfc3339-formatted UTC timestamp, or `null` if the reconcile
    /// loop has not yet ticked once. (Realistic only for the first
    /// ~30s after boot.)
    last_tick_at: Option<String>,
}

pub(super) async fn healthz(state: State<Arc<AppState>>) -> Json<HealthzResponse> {
    let last = *state.last_tick_at.read().await;
    Json(HealthzResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        last_tick_at: last.map(|t| t.to_rfc3339()),
    })
}

#[derive(Debug, Serialize)]
pub(super) struct WhoamiResponse {
    cn: String,
    /// rfc3339-formatted timestamp the server received the request.
    /// `issuedAt` semantically refers to "the moment we observed
    /// this verified identity", not the cert's notBefore.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` — returns the verified mTLS CN of the caller.
pub(super) async fn whoami(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
) -> Result<Json<WhoamiResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    Ok(Json(WhoamiResponse {
        cn,
        issued_at: Utc::now().to_rfc3339(),
    }))
}

/// `POST /v1/agent/checkin` — record an agent checkin.
///
/// Validates the body's `hostname` matches the verified mTLS CN
/// (sanity check, not a security boundary — the CN was already
/// authenticated by WebPkiClientVerifier; this just catches
/// configuration drift like a host using the wrong cert).
///
/// Emits a journal line per checkin so operators can grep
/// `journalctl -u nixfleet-control-plane | grep checkin`.
pub(super) async fn checkin(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<CheckinRequest>,
) -> Result<Json<CheckinResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "checkin rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let last_fetch = req
        .last_fetch_outcome
        .as_ref()
        .map(|o| format!("{:?}", o.result).to_lowercase())
        .unwrap_or_else(|| "none".to_string());
    let pending = req
        .pending_generation
        .as_ref()
        .map(|p| p.closure_hash.as_str())
        .unwrap_or("null");
    tracing::info!(
        target: "checkin",
        hostname = %req.hostname,
        closure_hash = %req.current_generation.closure_hash,
        pending = %pending,
        last_fetch = %last_fetch,
        "checkin received"
    );

    let now = Utc::now();
    let record = HostCheckinRecord {
        last_checkin: now,
        checkin: req.clone(),
    };
    state
        .host_checkins
        .write()
        .await
        .insert(req.hostname.clone(), record);

    let target = dispatch_target_for_checkin(&state, &req, now).await;

    Ok(Json(CheckinResponse {
        target,
        next_checkin_secs: NEXT_CHECKIN_SECS,
    }))
}

/// Per-checkin dispatch decision. Reads the latest verified
/// `FleetResolved` from `AppState`, queries the DB for any pending
/// confirm row for this host (idempotency guard), and runs
/// `dispatch::decide_target`. On `Dispatch`, inserts a
/// `pending_confirms` row keyed on the deterministic rollout id and
/// returns the target. All other Decision variants resolve to None.
///
/// Failures here log + return None — a transient DB hiccup or
/// missing fleet snapshot must not surface as HTTP 500 to the
/// agent. The agent retries on its next checkin (60s).
async fn dispatch_target_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let Some(db) = state.db.as_ref() else {
        return None;
    };
    let fleet_snapshot = state.verified_fleet.read().await.clone();
    let Some(fleet) = fleet_snapshot else {
        return None;
    };
    let pending_for_host = match db.pending_confirm_exists(&req.hostname) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "dispatch: pending_confirm_exists query failed",
            );
            return None;
        }
    };

    let decision = crate::dispatch::decide_target(
        &req.hostname,
        req,
        &fleet,
        pending_for_host,
        now,
        CONFIRM_DEADLINE_SECS as u32,
    );

    match decision {
        crate::dispatch::Decision::Dispatch {
            target,
            rollout_id,
            wave_index,
        } => {
            let confirm_deadline = now + chrono::Duration::seconds(CONFIRM_DEADLINE_SECS);
            match db.record_pending_confirm(
                &req.hostname,
                &rollout_id,
                /* wave */ wave_index.unwrap_or(0),
                &target.closure_hash,
                &target.channel_ref,
                confirm_deadline,
            ) {
                Ok(_) => {
                    tracing::info!(
                        target: "dispatch",
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        target_closure = %target.closure_hash,
                        confirm_deadline = %confirm_deadline.to_rfc3339(),
                        "dispatch: target issued",
                    );
                    Some(target)
                }
                Err(err) => {
                    tracing::warn!(
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        error = %err,
                        "dispatch: record_pending_confirm failed; returning no target",
                    );
                    None
                }
            }
        }
        other => {
            tracing::debug!(
                target: "dispatch",
                hostname = %req.hostname,
                decision = ?other,
                "dispatch: no target",
            );
            None
        }
    }
}

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

    tracing::info!(
        target: "report",
        hostname = %req.hostname,
        event = %event_str,
        rollout = %rollout_str,
        agent_version = %req.agent_version,
        event_id = %event_id,
        "report received"
    );

    let record = ReportRecord {
        event_id: event_id.clone(),
        received_at,
        report: req.clone(),
    };
    let mut reports = state.host_reports.write().await;
    let buf = reports.entry(req.hostname).or_default();
    if buf.len() >= REPORT_RING_CAP {
        buf.pop_front();
    }
    buf.push_back(record);

    Ok(Json(ReportResponse { event_id }))
}

/// 8-char lowercase-alnum suffix for event IDs. Not crypto-grade —
/// just enough to make IDs visually distinct in journal output.
fn rand_suffix(n: usize) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(n);
    let mut x = nanos.wrapping_mul(0x9e3779b97f4a7c15);
    for _ in 0..n {
        let idx = (x % alphabet.len() as u64) as usize;
        out.push(alphabet[idx] as char);
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    out
}

/// `POST /v1/enroll` — bootstrap a new fleet host.
///
/// No mTLS required (this is the path before the host has a cert).
/// Authentication is via the bootstrap-token signature against the
/// org root key in trust.json. Order of checks matches RFC-0003 §2:
/// 1. Replay defense
/// 2. Expiry
/// 3. Signature against `orgRootKey.{current,previous}`
/// 4. Hostname binding (claim ↔ CSR CN)
/// 5. Pubkey-fingerprint binding (SHA-256 of CSR pubkey DER)
pub(super) async fn enroll(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, StatusCode> {
    use base64::Engine;

    let now = chrono::Utc::now();

    // 1. Replay defense.
    if let Some(db) = &state.db {
        match db.token_seen(&req.token.claims.nonce) {
            Ok(true) => {
                tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected (db)");
                return Err(StatusCode::CONFLICT);
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!(error = %err, "enroll: db token_seen failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    } else {
        let seen = state.seen_token_nonces.read().await;
        if seen.contains(&req.token.claims.nonce) {
            tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected (mem)");
            return Err(StatusCode::CONFLICT);
        }
    }

    // 2. Expiry.
    if now < req.token.claims.issued_at || now >= req.token.claims.expires_at {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            "enroll: token outside validity window"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 3. Signature against orgRootKey. Re-read trust.json each
    // enroll so operator key rotations propagate without restart.
    let trust_path = state
        .issuance_paths
        .read()
        .await
        .fleet_ca_cert
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/nixfleet/cp"))
        .join("trust.json");
    let trust_raw = std::fs::read_to_string(&trust_path).map_err(|err| {
        tracing::error!(error = %err, path = %trust_path.display(), "enroll: read trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).map_err(|err| {
        tracing::error!(error = %err, "enroll: parse trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let org_root = trust.org_root_key.as_ref().ok_or_else(|| {
        tracing::error!(
            "enroll: trust.json has no orgRootKey — refusing to accept any token. \
             Set nixfleet.trust.orgRootKey.current in fleet.nix and rebuild."
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let candidates = org_root.active_keys();
    if candidates.is_empty() {
        tracing::error!("enroll: orgRootKey has no current/previous keys");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let mut sig_ok = false;
    for pubkey in &candidates {
        if pubkey.algorithm != "ed25519" {
            tracing::warn!(
                algorithm = %pubkey.algorithm,
                "enroll: skipping non-ed25519 orgRootKey candidate (only ed25519 supported)"
            );
            continue;
        }
        let pubkey_bytes = match base64::engine::general_purpose::STANDARD.decode(&pubkey.public) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(error = %err, "enroll: orgRootKey base64 decode");
                continue;
            }
        };
        if crate::issuance::verify_token_signature(&req.token, &pubkey_bytes).is_ok() {
            sig_ok = true;
            break;
        }
    }
    if !sig_ok {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            nonce = %req.token.claims.nonce,
            "enroll: token signature did not verify against any orgRootKey candidate"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 4. Hostname / 5. pubkey-fingerprint validation against CSR.
    let csr_params =
        rcgen::CertificateSigningRequestParams::from_pem(&req.csr_pem).map_err(|err| {
            tracing::warn!(error = %err, "enroll: parse CSR PEM");
            StatusCode::BAD_REQUEST
        })?;
    let csr_cn: Option<String> = csr_params
        .params
        .distinguished_name
        .iter()
        .find_map(|(t, v): (&rcgen::DnType, &rcgen::DnValue)| {
            if matches!(t, rcgen::DnType::CommonName) {
                Some(match v {
                    rcgen::DnValue::PrintableString(s) => s.to_string(),
                    rcgen::DnValue::Utf8String(s) => s.to_string(),
                    _ => format!("{:?}", v),
                })
            } else {
                None
            }
        });
    let csr_cn = csr_cn.ok_or_else(|| {
        tracing::warn!("enroll: CSR has no CN");
        StatusCode::BAD_REQUEST
    })?;
    let csr_pubkey_der = csr_params.public_key.der_bytes();
    let csr_fingerprint = crate::issuance::fingerprint(csr_pubkey_der);

    if let Err(err) = crate::issuance::validate_token_claims(
        &req.token.claims,
        &csr_cn,
        &csr_fingerprint,
        now,
    ) {
        tracing::warn!(error = %err, hostname = %req.token.claims.hostname, "enroll: claim validation");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // All checks passed — commit the nonce as seen.
    if let Some(db) = &state.db {
        if let Err(err) = db.record_token_nonce(&req.token.claims.nonce, &req.token.claims.hostname) {
            tracing::warn!(error = %err, "enroll: db record_token_nonce failed; proceeding");
        }
    } else {
        state
            .seen_token_nonces
            .write()
            .await
            .insert(req.token.claims.nonce.clone());
    }

    // Issue the cert.
    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => {
            tracing::error!("enroll: fleet CA cert/key paths not configured");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "enroll: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            "<enroll>",
            &req.token.claims.hostname,
            not_after,
            &crate::issuance::AuditContext::Enroll {
                token_nonce: req.token.claims.nonce.clone(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %req.token.claims.hostname,
        not_after = %not_after.to_rfc3339(),
        "enrolled"
    );

    Ok(Json(EnrollResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/renew` — issue a fresh cert for an authenticated
/// agent. mTLS-required; the verified CN is stamped onto the new
/// cert via `issuance::issue_cert`.
pub(super) async fn renew(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<RenewRequest>,
) -> Result<Json<RenewResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    let now = chrono::Utc::now();

    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "renew: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            &cn,
            &cn,
            not_after,
            &crate::issuance::AuditContext::Renew {
                previous_cert_serial: "<unknown>".to_string(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %cn,
        not_after = %not_after.to_rfc3339(),
        "renewed"
    );

    Ok(Json(RenewResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/confirm` — agent confirms successful activation.
/// Marks the matching `pending_confirms` row as confirmed.
///
/// Behaviour:
/// - Pending row exists, deadline not passed → mark confirmed, 204.
/// - No matching row in 'pending' state → 410 Gone (covers both
///   "wrong rollout_id" and "deadline expired / cancelled"; agent
///   responds the same way: trigger local rollback per RFC §4.2).
/// - DB unset → 503 (endpoint requires persistence).
pub(super) async fn confirm(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ConfirmRequest>,
) -> Result<Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "confirm rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let db = state.db.as_ref().ok_or_else(|| {
        tracing::warn!("confirm: no db configured — endpoint unusable");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let updated = db.confirm_pending(&req.hostname, &req.rollout).map_err(|err| {
        tracing::error!(error = %err, "confirm: db update failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if updated == 0 {
        tracing::info!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            "confirm: no matching pending row — returning 410"
        );
        return Ok(Response::builder()
            .status(StatusCode::GONE)
            .body(Body::from(""))
            .unwrap_or_default());
    }

    tracing::info!(
        target: "confirm",
        hostname = %req.hostname,
        rollout = %req.rollout,
        wave = req.wave,
        closure_hash = %req.generation.closure_hash,
        "confirm received"
    );
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::from(""))
        .unwrap_or_default())
}

/// `GET /v1/agent/closure/{hash}` — closure proxy fallback for hosts
/// that can't reach the binary cache directly. Forwards narinfo
/// requests to the configured cache upstream (any nix-cache-protocol
/// HTTP backend: harmonia, attic, cachix, plain nix-serve, …). Real
/// Nix-cache-protocol forwarding (full nar streaming) is a follow-up
/// PR; this lands the wire shape + the upstream config path.
///
/// When `closure_upstream` is unset, returns 501 Not Implemented.
pub(super) async fn closure_proxy(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Path(closure_hash): Path<String>,
) -> Result<Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;

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
                .unwrap_or_default());
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
                .unwrap_or_default());
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
        .unwrap_or_default())
}
