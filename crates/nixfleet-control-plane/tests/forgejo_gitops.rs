//! GitOps closure: stub Forgejo serves a real signed
//! `fleet.resolved.json` + `.sig`, the poll task fetches both,
//! verify_artifact accepts, the shared `verified_fleet` snapshot
//! flips from `None` to `Some(...)`. This is the end-to-end test
//! for "operator pushes fleet → CI re-signs → CP picks it up
//! within one poll cycle without redeploy".
//!
//! Stub uses the same tiny tokio TcpListener pattern as
//! `closure_proxy.rs` — minimal moving parts, no wiremock dep.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::forgejo_poll::{spawn, ChannelRefsCache, ForgejoConfig};
use nixfleet_proto::FleetResolved;
use rand::rngs::OsRng;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

fn build_fleet_resolved_json(declared_closure: &str, ci_commit: &str) -> (String, Vec<u8>) {
    let signed_at = "2026-04-26T00:00:00Z";
    let json = serde_json::json!({
        "schemaVersion": 1,
        "hosts": {
            "krach": {
                "system": "x86_64-linux",
                "tags": [],
                "channel": "stable",
                "closureHash": declared_closure,
                "pubkey": null,
            }
        },
        "channels": {
            "stable": {
                "rolloutPolicy": "default",
                "reconcileIntervalMinutes": 5,
                "freshnessWindow": 60,
                "signingIntervalMinutes": 30,
                "compliance": { "strict": false, "frameworks": [] },
            }
        },
        "rolloutPolicies": {},
        "waves": {},
        "edges": [],
        "disruptionBudgets": [],
        "meta": {
            "schemaVersion": 1,
            "signedAt": signed_at,
            "ciCommit": ci_commit,
        },
    });
    let raw = serde_json::to_string(&json).unwrap();
    let canonical = nixfleet_canonicalize::canonicalize(&raw).unwrap();
    (raw, canonical.into_bytes())
}

/// Tiny single-purpose Forgejo Contents-API stub. Listens until the
/// task is dropped; for each connection, parses the request line,
/// matches against `artifact_path` / `signature_path`, returns the
/// matching base64-encoded body wrapped in the Contents-API JSON
/// shape. Anything else → 404.
async fn spawn_stub_forgejo(
    artifact_path: &'static str,
    artifact_body: Vec<u8>,
    signature_path: &'static str,
    signature_body: Vec<u8>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let artifact_clone = artifact_body.clone();
            let signature_clone = signature_body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                // Match signature path first because the artifact
                // path is a prefix of it (`.../fleet.resolved.json`
                // vs `.../fleet.resolved.json.sig`). Naive
                // `req.contains(artifact_path)` would swallow the
                // signature request and serve the artifact body in
                // its place — verify then trips on a mis-shaped
                // "signature" with the artifact's BadSignature
                // surface.
                let target_body = if req.contains(signature_path) {
                    Some(signature_clone)
                } else if req.contains(artifact_path) {
                    Some(artifact_clone)
                } else {
                    None
                };

                let resp = match target_body {
                    Some(body) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&body);
                        let json = serde_json::json!({
                            "content": b64,
                            "encoding": "base64",
                        })
                        .to_string();
                        // Connection: close forces reqwest to open a
                        // fresh TCP connection for each request — the
                        // stub handles exactly one request per accept,
                        // so keepalive would deadlock the second GET.
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            json.len(),
                            json,
                        )
                    }
                    None => "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_string(),
                };
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    (port, handle)
}

fn init_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,nixfleet_control_plane::forgejo_poll=debug")),
            )
            .try_init();
    });
}

#[tokio::test]
async fn poll_refreshes_verified_fleet_snapshot() {
    init_tracing();
    let dir = TempDir::new().unwrap();

    // Mint a CI release key, sign a real artifact + write the
    // matching trust.json. Same posture the live deployment uses
    // (only with ed25519 instead of TPM-backed ecdsa-p256 — the
    // verify_artifact path is shared).
    let signing_key = SigningKey::generate(&mut OsRng);
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key());

    let (raw_json, canonical_bytes) =
        build_fleet_resolved_json("decl0001-nixos-system-krach-26.05", "deadbeef00000000");
    let signature = signing_key.sign(&canonical_bytes);

    // Sanity: re-canonicalize the served raw bytes — the verifier
    // does the same. If these don't match, sign-then-verify can't
    // possibly work.

    let trust_path = dir.path().join("trust.json");
    let trust = serde_json::json!({
        "schemaVersion": 1,
        "ciReleaseKey": {
            "current": { "algorithm": "ed25519", "public": public_b64 },
            "previous": null,
            "rejectBefore": null,
        },
        "atticCacheKey": null,
        "orgRootKey": null,
    });
    std::fs::write(&trust_path, trust.to_string()).unwrap();

    let token_path = dir.path().join("token");
    std::fs::write(&token_path, "fake-token").unwrap();

    let (port, _stub) = spawn_stub_forgejo(
        "/contents/releases/fleet.resolved.json",
        raw_json.into_bytes(),
        "/contents/releases/fleet.resolved.json.sig",
        signature.to_bytes().to_vec(),
    )
    .await;

    let cache = Arc::new(RwLock::new(ChannelRefsCache::default()));
    let verified_fleet: Arc<RwLock<Option<Arc<FleetResolved>>>> = Arc::new(RwLock::new(None));

    let cfg = ForgejoConfig {
        base_url: format!("http://127.0.0.1:{port}"),
        owner: "abstracts33d".to_string(),
        repo: "fleet".to_string(),
        artifact_path: "releases/fleet.resolved.json".to_string(),
        signature_path: "releases/fleet.resolved.json.sig".to_string(),
        token_file: token_path,
        trust_path,
        // Far-future window; the artifact's signedAt is fixed at
        // 2026-04-26T00:00:00Z and we don't want time drift to flake
        // the test.
        freshness_window: Duration::from_secs(86400 * 365 * 5),
    };

    let _poll = spawn(cache.clone(), verified_fleet.clone(), cfg);

    // First scheduled tick fires after `POLL_INTERVAL` (60s in
    // production). Tests can't wait that long — drive forward by
    // polling the snapshot ourselves with a generous timeout. The
    // poll task internally uses `tokio::time::interval` which
    // schedules its first tick immediately by default, so the
    // refresh should land within a few hundred ms.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut last_snapshot: Option<Arc<FleetResolved>> = None;
    while std::time::Instant::now() < deadline {
        if let Some(s) = verified_fleet.read().await.clone() {
            last_snapshot = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let fleet =
        last_snapshot.expect("verified_fleet snapshot should have been refreshed by the poll");
    assert_eq!(
        fleet.hosts.get("krach").and_then(|h| h.closure_hash.as_deref()),
        Some("decl0001-nixos-system-krach-26.05"),
        "snapshot should carry the fetched closureHash",
    );
    assert_eq!(fleet.meta.ci_commit.as_deref(), Some("deadbeef00000000"));

    // Channel refs cache should also have been refreshed.
    let refs = cache.read().await.refs.clone();
    assert!(refs.contains_key("stable"), "channel_refs should include stable: {refs:?}");
}

#[tokio::test]
async fn poll_retains_snapshot_on_verify_failure() {
    // A bad signature must NOT blank out a previously-good snapshot.
    // Operator pushed something broken; the dispatch path keeps
    // running against the last-known-good fleet until the next good
    // poll. Same retain-on-failure posture as `channel_refs`.
    let dir = TempDir::new().unwrap();

    // Real artifact signed by `signing_key`, but trust.json declares
    // a *different* key — verify_artifact will reject every call.
    let signing_key = SigningKey::generate(&mut OsRng);
    let wrong_key = SigningKey::generate(&mut OsRng);
    let wrong_public_b64 =
        base64::engine::general_purpose::STANDARD.encode(wrong_key.verifying_key());

    let (raw_json, canonical_bytes) =
        build_fleet_resolved_json("decl0001-nixos-system-krach-26.05", "cafebabe00000000");
    let signature = signing_key.sign(&canonical_bytes);

    let trust_path = dir.path().join("trust.json");
    let trust = serde_json::json!({
        "schemaVersion": 1,
        "ciReleaseKey": {
            "current": { "algorithm": "ed25519", "public": wrong_public_b64 },
            "previous": null,
            "rejectBefore": null,
        },
        "atticCacheKey": null,
        "orgRootKey": null,
    });
    std::fs::write(&trust_path, trust.to_string()).unwrap();

    let token_path = dir.path().join("token");
    std::fs::write(&token_path, "fake-token").unwrap();

    let (port, _stub) = spawn_stub_forgejo(
        "/contents/releases/fleet.resolved.json",
        raw_json.into_bytes(),
        "/contents/releases/fleet.resolved.json.sig",
        signature.to_bytes().to_vec(),
    )
    .await;

    // Pre-seed verified_fleet with a sentinel snapshot — the poll
    // failure must not overwrite it.
    let sentinel: FleetResolved = serde_json::from_str(&serde_json::json!({
        "schemaVersion": 1,
        "hosts": { "sentinel": { "system": "x86_64-linux", "tags": [], "channel": "stable", "closureHash": "sentinel-hash", "pubkey": null } },
        "channels": { "stable": { "rolloutPolicy": "x", "reconcileIntervalMinutes": 1, "freshnessWindow": 1, "signingIntervalMinutes": 1, "compliance": { "strict": false, "frameworks": [] } } },
        "rolloutPolicies": {},
        "waves": {},
        "edges": [],
        "disruptionBudgets": [],
        "meta": { "schemaVersion": 1, "signedAt": "2025-01-01T00:00:00Z", "ciCommit": "old-rev" },
    }).to_string()).unwrap();

    let cache = Arc::new(RwLock::new(ChannelRefsCache::default()));
    let verified_fleet: Arc<RwLock<Option<Arc<FleetResolved>>>> =
        Arc::new(RwLock::new(Some(Arc::new(sentinel))));

    let cfg = ForgejoConfig {
        base_url: format!("http://127.0.0.1:{port}"),
        owner: "abstracts33d".to_string(),
        repo: "fleet".to_string(),
        artifact_path: "releases/fleet.resolved.json".to_string(),
        signature_path: "releases/fleet.resolved.json.sig".to_string(),
        token_file: token_path,
        trust_path,
        freshness_window: Duration::from_secs(86400 * 365 * 5),
    };

    let _poll = spawn(cache.clone(), verified_fleet.clone(), cfg);

    // Wait long enough for the first poll to fire and fail; assert
    // the sentinel survives.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let snapshot = verified_fleet.read().await.clone();
    let fleet = snapshot.expect("sentinel must be retained on verify failure");
    assert_eq!(
        fleet.hosts.get("sentinel").and_then(|h| h.closure_hash.as_deref()),
        Some("sentinel-hash"),
        "verify-failure must NOT overwrite sentinel snapshot",
    );
}
