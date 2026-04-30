//! `GET /v1/rollouts/<rolloutId>` and `GET /v1/rollouts/<rolloutId>/sig`.
//!
//! Stateless distributor for the pre-signed rollout manifests
//! produced by `nixfleet-release` (RFC-0002 §4.4 / RFC-0003 §4.6).
//! The CP holds NO signing key for rollouts — these handlers serve
//! the on-disk pre-signed bytes byte-for-byte. The agent performs the
//! signature verification on receipt: it has the trust roots locally
//! and that's the load-bearing check.
//!
//! The CP does ONE local check: recompute the canonical-bytes hash of
//! the on-disk manifest and assert it equals the `<rolloutId>` from
//! the URL. The pair is content-addressed (filename IS the hash), so
//! a mismatch means the on-disk file was renamed or corrupted and the
//! CP refuses to spread the inconsistency.
//!
//! Failure modes:
//! - 503 — `rollouts_dir` is None (CP started without manifest distribution).
//! - 404 — file not found, or `<rolloutId>` is not 64-char hex.
//! - 500 — files present but recomputed hash doesn't match the path.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;

use super::state::AppState;

/// SHA-256 hex is exactly 64 lowercase chars. Reject anything else
/// fast — saves a filesystem syscall on bogus paths and prevents
/// path-traversal smuggling (`..`, NUL, etc. fail the hex check).
fn looks_like_rollout_id(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn manifest_paths(dir: &PathBuf, rollout_id: &str) -> (PathBuf, PathBuf) {
    let manifest = dir.join(format!("{rollout_id}.json"));
    let sig = dir.join(format!("{rollout_id}.json.sig"));
    (manifest, sig)
}

fn load_pair(state: &AppState, rollout_id: &str) -> Result<(Vec<u8>, Vec<u8>), StatusCode> {
    let dir = state.rollouts_dir.as_ref().ok_or_else(|| {
        tracing::debug!(
            rollout_id = %rollout_id,
            "rollouts handler: rollouts_dir not configured; returning 503",
        );
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    if !looks_like_rollout_id(rollout_id) {
        return Err(StatusCode::NOT_FOUND);
    }

    let (manifest_path, sig_path) = manifest_paths(dir, rollout_id);
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(StatusCode::NOT_FOUND);
        }
        Err(err) => {
            tracing::warn!(
                rollout_id = %rollout_id,
                path = %manifest_path.display(),
                error = %err,
                "rollouts handler: read manifest failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let sig_bytes = match std::fs::read(&sig_path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Manifest present but signature missing — operator
            // mid-deploy or filesystem inconsistency. Don't serve the
            // unverifiable manifest.
            tracing::warn!(
                rollout_id = %rollout_id,
                "rollouts handler: signature file missing for present manifest",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(err) => {
            tracing::warn!(
                rollout_id = %rollout_id,
                error = %err,
                "rollouts handler: read signature failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Content-address sanity: the rolloutId in the URL must be the
    // sha256 hex of the on-disk manifest. A mismatch means the file
    // was renamed or corrupted; the CP refuses to spread it. Cheap
    // (one sha256 over a small file), no trust roots needed.
    let canonical = match std::str::from_utf8(&manifest_bytes) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                rollout_id = %rollout_id,
                "rollouts handler: manifest bytes are not valid UTF-8",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let parsed: nixfleet_proto::RolloutManifest =
        serde_json::from_str(canonical).map_err(|err| {
            tracing::warn!(
                rollout_id = %rollout_id,
                error = %err,
                "rollouts handler: manifest does not parse",
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let recomputed = nixfleet_reconciler::compute_rollout_id(&parsed).map_err(|err| {
        tracing::warn!(
            rollout_id = %rollout_id,
            error = ?err,
            "rollouts handler: recompute_rollout_id failed",
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if recomputed != rollout_id {
        tracing::warn!(
            rollout_id = %rollout_id,
            recomputed = %recomputed,
            "rollouts handler: on-disk manifest hash does not match path — refusing to serve",
        );
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok((manifest_bytes, sig_bytes))
}

/// `GET /v1/rollouts/{rolloutId}` — returns the canonical manifest
/// bytes as `application/json`.
pub(super) async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (manifest_bytes, _sig) = load_pair(&state, &rollout_id)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(manifest_bytes)))
}

/// `GET /v1/rollouts/{rolloutId}/sig` — returns the raw signature
/// bytes as `application/octet-stream`.
pub(super) async fn signature(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (_manifest, sig_bytes) = load_pair(&state, &rollout_id)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(sig_bytes)))
}
