//! Stateless distributor for the signed `fleet.resolved.json` artifact.
//!
//! Serves the canonical signed bytes from `state.verified_fleet` (the
//! `cp_manifest_poll` worker's in-memory snapshot). The bytes returned
//! are exactly the bytes CP fetched from the channel-refs source and
//! verified against the trust roots; signature verification + the
//! rollout-anchored `fleet_resolved_hash` discriminator happen at the
//! consumer (per RFC-0011 §1 invariant #1 — single signed source of
//! truth + defense-in-depth at the verification gate).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;

use super::super::state::AppState;

/// `GET /v1/fleet.resolved` - canonical signed bytes; mTLS via the
/// router-level `require_cn_layer`.
pub(in crate::server) async fn artifact(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let snapshot_guard = state.verified_fleet.read().await;
    let snapshot = snapshot_guard
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bytes = Bytes::copy_from_slice(&snapshot.artifact_bytes);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, bytes))
}

/// `GET /v1/fleet.resolved/sig` - raw signature bytes.
pub(in crate::server) async fn signature(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let snapshot_guard = state.verified_fleet.read().await;
    let snapshot = snapshot_guard
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bytes = Bytes::copy_from_slice(&snapshot.signature_bytes);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::VerifiedFleetSnapshot;
    use nixfleet_proto::FleetResolved;
    use std::sync::Arc;

    fn state_with_snapshot(snapshot: Option<VerifiedFleetSnapshot>) -> Arc<AppState> {
        let state = AppState {
            verified_fleet: Arc::new(tokio::sync::RwLock::new(snapshot)),
            ..Default::default()
        };
        Arc::new(state)
    }

    fn sample_snapshot(artifact: &[u8], signature: &[u8]) -> VerifiedFleetSnapshot {
        // Minimal valid FleetResolved; the route tests only exercise the
        // bytes path so the fleet's content is opaque here.
        let fleet: FleetResolved = serde_json::from_str(
            r#"{"schemaVersion":1,"hosts":{},"channels":{},"waves":{},"meta":{"schemaVersion":1}}"#,
        )
        .expect("minimal FleetResolved JSON valid");
        VerifiedFleetSnapshot {
            fleet: Arc::new(fleet),
            fleet_resolved_hash: "0".repeat(64),
            artifact_bytes: artifact.to_vec(),
            signature_bytes: signature.to_vec(),
        }
    }

    #[tokio::test]
    async fn artifact_returns_503_when_verified_fleet_unset() {
        let state = state_with_snapshot(None);
        match artifact(State(state)).await {
            Err(status) => assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE),
            Ok(_) => panic!("expected SERVICE_UNAVAILABLE when verified_fleet is None"),
        }
    }

    #[tokio::test]
    async fn signature_returns_503_when_verified_fleet_unset() {
        let state = state_with_snapshot(None);
        match signature(State(state)).await {
            Err(status) => assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE),
            Ok(_) => panic!("expected SERVICE_UNAVAILABLE when verified_fleet is None"),
        }
    }

    #[tokio::test]
    async fn artifact_returns_snapshot_bytes_on_200() {
        let snap = sample_snapshot(br#"{"hello":"fleet"}"#, &[0xDE, 0xAD]);
        let state = state_with_snapshot(Some(snap));
        let resp = artifact(State(state))
            .await
            .expect("artifact returns Ok")
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], br#"{"hello":"fleet"}"#);
    }

    #[tokio::test]
    async fn signature_returns_snapshot_bytes_on_200() {
        let snap = sample_snapshot(b"{}", &[0xDE, 0xAD, 0xBE, 0xEF]);
        let state = state_with_snapshot(Some(snap));
        let resp = signature(State(state))
            .await
            .expect("signature returns Ok")
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        assert_eq!(&body[..], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}
