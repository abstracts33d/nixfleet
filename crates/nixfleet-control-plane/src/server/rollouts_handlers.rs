//! `GET /v1/rollouts/<rolloutId>` and `GET /v1/rollouts/<rolloutId>/sig`.
//!
//! Stateless distributor for the pre-signed rollout manifests
//! produced by `nixfleet-release` (RFC-0002 §4.4 / RFC-0003 §4.6).
//! The CP holds NO signing key for rollouts — these handlers serve
//! pre-signed bytes byte-for-byte. The agent performs the signature
//! verification on receipt: it has the trust roots locally and that's
//! the load-bearing check.
//!
//! Two sources, tried in order:
//! 1. `rollouts_dir` — filesystem path (offline / air-gapped fleets,
//!    or fleets that succeed the bootstrap-without-rebuild).
//! 2. `rollouts_source` — HTTP fetch from a configured URL pair
//!    (typical case, since `nixfleet-release` writes manifests AFTER
//!    building closures and so they aren't in `inputs.self` for the
//!    closure being activated).
//!
//! The CP does ONE local check regardless of source: recompute the
//! sha256 of the manifest bytes and assert it equals `<rolloutId>`.
//! Filename IS the hash, so a mismatch means corruption (filesystem)
//! or upstream serving the wrong bytes (HTTP); the CP refuses to
//! spread either.
//!
//! Failure modes:
//! - 503 — both `rollouts_dir` AND `rollouts_source` are None.
//! - 404 — `<rolloutId>` is not 64-char hex, or both sources missed.
//! - 500 — bytes present but recomputed hash doesn't match the path.

use std::path::{Path as FsPath, PathBuf};
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
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn manifest_paths(dir: &FsPath, rollout_id: &str) -> (PathBuf, PathBuf) {
    let manifest = dir.join(format!("{rollout_id}.json"));
    let sig = dir.join(format!("{rollout_id}.json.sig"));
    (manifest, sig)
}

/// `(manifest_bytes, signature_bytes)` for a rollout id. Aliased to
/// keep the source-fallback signatures readable.
type ManifestPair = (Vec<u8>, Vec<u8>);

/// Try the filesystem path. Returns:
/// - `Ok(Some((manifest, sig)))` — both files present.
/// - `Ok(None)` — manifest absent (try next source).
/// - `Err(...)` — manifest present but sig missing, or read errored.
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
    Ok(Some((manifest_bytes, sig_bytes)))
}

/// Recompute the canonical sha256 of the manifest bytes and assert
/// it equals `rolloutId`. The pair is content-addressed (filename IS
/// the hash) regardless of source, so a mismatch means corruption
/// (filesystem) or wrong-bytes-for-id (HTTP); CP refuses to spread.
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

    // 1. Try filesystem first — air-gapped fleets and the fast path
    //    when manifests do happen to land in `inputs.self`.
    if let Some(dir) = state.rollouts_dir.as_ref() {
        if let Some((manifest_bytes, sig_bytes)) = try_load_from_dir(dir, rollout_id)? {
            verify_content_address(&manifest_bytes, rollout_id)?;
            return Ok((manifest_bytes, sig_bytes));
        }
    }

    // 2. Filesystem missed — try HTTP source.
    if let Some(source) = state.rollouts_source.as_ref() {
        match source.fetch_pair(rollout_id).await {
            Ok((manifest_bytes, sig_bytes)) => {
                // `fetch_pair` already verifies the raw-bytes sha256
                // against `rolloutId`. We re-run the canonical-form
                // check (parses + recomputes via `compute_rollout_id`)
                // for parity with the filesystem path — same defence
                // against a malformed-but-correctly-hashed payload.
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
                // Distinguish "upstream said 404" from infra error so
                // the agent's retry policy can differ. We don't have
                // the structured status here, so 502 BAD GATEWAY for
                // anything that wasn't a clean filesystem-then-source
                // miss; agents already treat 5xx as transient.
                return Err(StatusCode::BAD_GATEWAY);
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

/// `GET /v1/rollouts/{rolloutId}` — returns the canonical manifest
/// bytes as `application/json`.
pub(super) async fn manifest(
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

/// `GET /v1/rollouts/{rolloutId}/sig` — returns the raw signature
/// bytes as `application/octet-stream`.
pub(super) async fn signature(
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
