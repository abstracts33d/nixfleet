//! Cross-cutting auth + protocol middleware for the v1 router.
//!
//! Two functions, both consumed by `serve.rs`'s router builder:
//!
//! - [`require_cn`] — extract the verified mTLS CN from the request
//!   extensions, enforce cert revocation when the DB is configured.
//!   Every `/v1/*` handler that gates on identity calls this first.
//! - [`protocol_version_middleware`] — protocol-version
//!   header enforcement on `/v1/*`. Forward-compat: missing header
//!   accepted with debug log; present+mismatched returns 426.

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::middleware::Next;
use nixfleet_proto::agent_wire::{PROTOCOL_MAJOR_VERSION, PROTOCOL_VERSION_HEADER};

use crate::auth::auth_cn::PeerCertificates;

use super::state::AppState;

/// Extract the verified CN from `PeerCertificates`, or return 401.
/// Also enforces cert revocation when `AppState.db` is set: a cert
/// whose notBefore predates the host's revocation entry is rejected
/// with 401. Re-enrolled certs (notBefore > revoked_before) pass.
///
/// Centralised so each `/v1/*` handler reads identity the same way.
pub(super) async fn require_cn(
    state: &AppState,
    peer_certs: &PeerCertificates,
) -> Result<String, StatusCode> {
    if !peer_certs.is_present() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let cn = peer_certs.leaf_cn().ok_or(StatusCode::UNAUTHORIZED)?;

    if let Some(db) = &state.db {
        match db.revocations().cert_revoked_before(&cn) {
            Ok(Some(revoked_before)) => {
                let cert_nbf = peer_certs
                    .leaf_not_before()
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                if cert_nbf < revoked_before {
                    tracing::warn!(
                        cn = %cn,
                        cert_not_before = %cert_nbf.to_rfc3339(),
                        revoked_before = %revoked_before.to_rfc3339(),
                        "rejecting revoked cert"
                    );
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
            Ok(None) => {} // not revoked
            Err(err) => {
                tracing::error!(error = %err, "db cert_revoked_before failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    Ok(cn)
}

/// Middleware: enforce `X-Nixfleet-Protocol: <PROTOCOL_MAJOR_VERSION>`
/// on `/v1/*` requests .
///
/// Forward-compat posture: missing header → log debug + accept. This
/// lets older agents that pre-date the version header keep working
/// during the transition. Header present + mismatched major → 426
/// Upgrade Required + log warn.
///
/// Strict mode (`AppState.strict`, opt-in via `--strict`): missing
/// header → 400 Bad Request, no forward-compat slack. See #70.
///
/// `/healthz` is not subject to this — it's the operator's status
/// probe and runs unauthenticated; protocol-versioning the health
/// check makes the operational debug story worse without buying
/// anything.
pub(super) async fn protocol_version_middleware(
    strict: bool,
    req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(value) = req.headers().get(PROTOCOL_VERSION_HEADER) {
        match value.to_str().ok().and_then(|s| s.parse::<u32>().ok()) {
            Some(v) if v == PROTOCOL_MAJOR_VERSION => Ok(next.run(req).await),
            Some(v) => {
                tracing::warn!(
                    sent = v,
                    expected = PROTOCOL_MAJOR_VERSION,
                    "rejecting request with mismatched protocol major version"
                );
                Err(StatusCode::UPGRADE_REQUIRED)
            }
            None => {
                tracing::warn!(
                    raw = ?value,
                    "X-Nixfleet-Protocol header malformed"
                );
                Err(StatusCode::BAD_REQUEST)
            }
        }
    } else if strict {
        tracing::warn!("rejecting request without X-Nixfleet-Protocol (strict mode)");
        Err(StatusCode::BAD_REQUEST)
    } else {
        tracing::debug!("request without X-Nixfleet-Protocol — accepting (forward-compat)");
        Ok(next.run(req).await)
    }
}
