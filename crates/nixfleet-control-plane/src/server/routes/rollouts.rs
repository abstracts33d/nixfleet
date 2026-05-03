//! Stateless distributor for pre-signed rollout manifests; CP holds no signing key.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;

use super::super::state::AppState;

// LOADBEARING: 64-char-hex check blocks path-traversal smuggling (`..`, NUL fail the hex check).
fn looks_like_rollout_id(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn manifest_paths(dir: &FsPath, rollout_id: &str) -> (PathBuf, PathBuf) {
    let manifest = dir.join(format!("{rollout_id}.json"));
    let sig = dir.join(format!("{rollout_id}.json.sig"));
    (manifest, sig)
}

type ManifestPair = (Vec<u8>, Vec<u8>);

fn try_load_from_dir(dir: &FsPath, rollout_id: &str) -> Result<Option<ManifestPair>, StatusCode> {
    let (manifest_path, sig_path) = manifest_paths(dir, rollout_id);
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
            // GOTCHA: manifest present but sig missing — refuse rather than serve unverifiable bytes.
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
    Ok(Some((manifest_bytes, sig_bytes)))
}

/// LOADBEARING: filename IS the sha256; mismatch means corruption or wrong-bytes-for-id.
fn verify_content_address(manifest_bytes: &[u8], rollout_id: &str) -> Result<(), StatusCode> {
    let canonical = std::str::from_utf8(manifest_bytes).map_err(|_| {
        tracing::warn!(
            rollout_id = %rollout_id,
            "rollouts handler: manifest bytes are not valid UTF-8",
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
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
            "rollouts handler: manifest hash does not match path — refusing to serve",
        );
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(())
}

async fn load_pair(state: &AppState, rollout_id: &str) -> Result<ManifestPair, StatusCode> {
    if state.rollouts_dir.is_none() && state.rollouts_source.is_none() {
        tracing::debug!(
            rollout_id = %rollout_id,
            "rollouts handler: neither rollouts_dir nor rollouts_source configured; returning 503",
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    if !looks_like_rollout_id(rollout_id) {
        return Err(StatusCode::NOT_FOUND);
    }

    if let Some(dir) = state.rollouts_dir.as_ref() {
        if let Some((manifest_bytes, sig_bytes)) = try_load_from_dir(dir, rollout_id)? {
            verify_content_address(&manifest_bytes, rollout_id)?;
            return Ok((manifest_bytes, sig_bytes));
        }
    }

    if let Some(source) = state.rollouts_source.as_ref() {
        match source.fetch_pair(rollout_id).await {
            Ok((manifest_bytes, sig_bytes)) => {
                // Parity with filesystem path: also defends against malformed-but-correctly-hashed payloads.
                verify_content_address(&manifest_bytes, rollout_id)?;
                tracing::info!(
                    rollout_id = %rollout_id,
                    "rollouts handler: fetched manifest pair from upstream source",
                );
                return Ok((manifest_bytes, sig_bytes));
            }
            Err(err) => {
                tracing::warn!(
                    rollout_id = %rollout_id,
                    error = %err,
                    "rollouts handler: upstream fetch failed",
                );
                return Err(StatusCode::BAD_GATEWAY);
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

/// `GET /v1/rollouts/{rolloutId}` — manifest bytes; mTLS via router-level `require_cn_layer`.
pub(in crate::server) async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (manifest_bytes, _sig) = load_pair(&state, &rollout_id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(manifest_bytes)))
}

/// `GET /v1/rollouts/{rolloutId}/sig` — raw signature bytes.
pub(in crate::server) async fn signature(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (_manifest, sig_bytes) = load_pair(&state, &rollout_id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(sig_bytes)))
}
