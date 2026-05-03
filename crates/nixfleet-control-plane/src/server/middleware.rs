//! Auth + protocol middleware for the v1 router.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::middleware::Next;
use nixfleet_proto::agent_wire::{PROTOCOL_MAJOR_VERSION, PROTOCOL_VERSION_HEADER};

use crate::auth::auth_cn::PeerCertificates;

use super::state::AppState;

/// 401 on missing/revoked cert; re-enrolled certs (notBefore > revoked_before) pass.
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
            Ok(None) => {}
            Err(err) => {
                tracing::error!(error = %err, "db cert_revoked_before failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    Ok(cn)
}

/// Type-system witness that auth ran; private field prevents forgery in handler code.
#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedCn(String);

impl AuthenticatedCn {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

pub(super) async fn require_cn_layer(
    state: Arc<AppState>,
    mut req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let peer_certs = req
        .extensions()
        .get::<PeerCertificates>()
        .cloned()
        .unwrap_or_default();
    let cn = require_cn(&state, &peer_certs).await?;
    req.extensions_mut().insert(AuthenticatedCn(cn));
    Ok(next.run(req).await)
}

/// Forward-compat: missing header accepted; mismatched major → 426. Strict mode rejects missing.
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
