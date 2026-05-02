//! Cert-issuance handlers: `/v1/enroll` (bootstrap) and
//! `/v1/agent/renew` (already-authenticated rotation).

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use nixfleet_proto::enroll_wire::{EnrollRequest, EnrollResponse, RenewRequest, RenewResponse};
use rcgen::PublicKeyData;

use crate::auth::auth_cn::PeerCertificates;

use super::super::middleware::require_cn;
use super::super::state::AppState;

/// `POST /v1/enroll` — bootstrap a new fleet host.
///
/// No mTLS required (this is the path before the host has a cert).
/// Authentication is via the bootstrap-token signature against the
/// org root key in trust.json. Order of checks matches :
/// 1. Replay defense
/// 2. Expiry
/// 3. Signature against `orgRootKey.{current,previous}`
/// 4. Hostname binding (claim ↔ CSR CN)
/// 5. Pubkey-fingerprint binding (SHA-256 of CSR pubkey DER)
pub(in crate::server) async fn enroll(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, StatusCode> {
    use base64::Engine;

    let now = chrono::Utc::now();

    let db = state.db.as_ref().ok_or_else(|| {
        tracing::warn!("enroll: no db configured — endpoint unusable");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    // 1. Replay defense.
    match db.tokens().token_seen(&req.token.claims.nonce) {
        Ok(true) => {
            tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected");
            return Err(StatusCode::CONFLICT);
        }
        Ok(false) => {}
        Err(err) => {
            tracing::error!(error = %err, "enroll: db token_seen failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
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
        if crate::auth::issuance::verify_token_signature(&req.token, &pubkey_bytes).is_ok() {
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
    let csr_cn: Option<String> = csr_params.params.distinguished_name.iter().find_map(
        |(t, v): (&rcgen::DnType, &rcgen::DnValue)| {
            if matches!(t, rcgen::DnType::CommonName) {
                Some(match v {
                    rcgen::DnValue::PrintableString(s) => s.to_string(),
                    rcgen::DnValue::Utf8String(s) => s.to_string(),
                    _ => format!("{:?}", v),
                })
            } else {
                None
            }
        },
    );
    let csr_cn = csr_cn.ok_or_else(|| {
        tracing::warn!("enroll: CSR has no CN");
        StatusCode::BAD_REQUEST
    })?;
    let csr_pubkey_der = csr_params.public_key.der_bytes();
    let csr_fingerprint = crate::auth::issuance::fingerprint(csr_pubkey_der);

    if let Err(err) = crate::auth::issuance::validate_token_claims(
        &req.token.claims,
        &csr_cn,
        &csr_fingerprint,
        now,
    ) {
        tracing::warn!(error = %err, hostname = %req.token.claims.hostname, "enroll: claim validation");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // All checks passed — atomically commit the nonce. The plain
    // `INSERT` (no `OR IGNORE`) returns `AlreadyRecorded` on PK
    // conflict, which closes the TOCTOU race between the early
    // `token_seen()` check above and this point. Two concurrent
    // /v1/enroll requests for the same nonce will both pass
    // `token_seen()`; only one will reach `Recorded` here. The
    // other gets 409 CONFLICT and never mints a cert. Genuine
    // DB failures (disk full, schema drift) return 500 instead of
    // silently proceeding (the old behaviour) so an unrecorded
    // nonce can never reach the cert-issuance path.
    match db
        .tokens()
        .record_token_nonce(&req.token.claims.nonce, &req.token.claims.hostname)
    {
        Ok(crate::db::RecordTokenOutcome::Recorded) => {}
        Ok(crate::db::RecordTokenOutcome::AlreadyRecorded) => {
            tracing::warn!(
                nonce = %req.token.claims.nonce,
                "enroll: token replay detected at record (concurrent enroll race or retry)",
            );
            return Err(StatusCode::CONFLICT);
        }
        Err(err) => {
            tracing::error!(error = %err, "enroll: db record_token_nonce failed; refusing enrollment");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
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
    let (cert_pem, not_after) = crate::auth::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::auth::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "enroll: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::auth::issuance::audit_log(
            path,
            now,
            "<enroll>",
            &req.token.claims.hostname,
            not_after,
            &crate::auth::issuance::AuditContext::Enroll {
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

    Ok(Json(EnrollResponse {
        cert_pem,
        not_after,
    }))
}

/// `POST /v1/agent/renew` — issue a fresh cert for an authenticated
/// agent. mTLS-required; the verified CN is stamped onto the new
/// cert via `issuance::issue_cert`.
pub(in crate::server) async fn renew(
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

    let (cert_pem, not_after) = crate::auth::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::auth::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "renew: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::auth::issuance::audit_log(
            path,
            now,
            &cn,
            &cn,
            not_after,
            &crate::auth::issuance::AuditContext::Renew {
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

    Ok(Json(RenewResponse {
        cert_pem,
        not_after,
    }))
}
