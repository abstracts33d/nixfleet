//! Dispatch-loop integration smoke test.
//!
//! Drives the full path that pure unit tests in `src/dispatch.rs`
//! cannot reach: a real ed25519-signed `fleet.resolved.json` is
//! verified at server boot (priming `AppState.verified_fleet`), an
//! agent checks in via mTLS, and the CP both issues a target on the
//! response AND writes a `host_dispatch_state` DB row. A second
//! checkin while that row is in flight must NOT re-dispatch
//! (idempotency gate `pending_dispatch_exists`).
//!
//! Decision-table coverage (Unmanaged, NoDeclaration, Converged,
//! InFlight, HoldAfterFailure, rollout-id derivation, wave-index
//! lookup, confirm-window threading) lives in
//! `src/dispatch.rs::tests` as pure unit tests. This file exists
//! solely to exercise the wiring those tests can't see: signed-
//! fleet verification, the checkin handler's DB side effect, and
//! the live idempotency gate against a real sqlite store.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use common::{
    build_mtls_client, install_crypto_provider_once, mint_ca_and_certs, pick_free_port,
    write_bytes, write_pem,
};
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::server;
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, FetchOutcome, FetchResult, GenerationRef,
};
use rand::rngs::OsRng;
use tempfile::TempDir;
use tokio::time::sleep;

/// Build a minimal valid `fleet.resolved.json` declaring `test-host`'s
/// channel + target closure. Returns the canonical bytes (sign these)
/// and the original JSON string (write to disk; the server re-
/// canonicalizes on read).
fn build_fleet_resolved_json(declared_closure: &str, ci_commit: &str) -> (String, Vec<u8>) {
    // Hand-rolled JSON to keep the test independent of the proto
    // crate's serde shape. Canonicalization will resort fields anyway,
    // so order is cosmetic.
    let signed_at = "2026-04-26T00:00:00Z";
    let json = serde_json::json!({
        "schemaVersion": 1,
        "hosts": {
            "test-host": {
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
                "compliance": { "mode": "disabled", "frameworks": [] },
            }
        },
        "rolloutPolicies": {
            "default": {
                "strategy": "waves",
                "waves": [],
                "healthGate": {},
                "onHealthFailure": "halt",
            }
        },
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

fn write_signed_fleet(
    dir: &TempDir,
    declared_closure: &str,
    ci_commit: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key());

    let (raw_json, canonical_bytes) = build_fleet_resolved_json(declared_closure, ci_commit);
    let signature = signing_key.sign(&canonical_bytes);

    let artifact = write_pem(dir, "fleet.resolved.json", &raw_json);
    let signature_path = write_bytes(dir, "fleet.resolved.json.sig", &signature.to_bytes());
    // Wide freshness via channels.freshnessWindow=60; trust.json also
    // needs to point at the test's pubkey so verify_artifact accepts.
    // KeySlot is `{current: Option<TrustedPubkey>, previous: Option<...>}`,
    // NOT a list. Required `schemaVersion` is part of the TrustConfig
    // wire shape (`nixfleet-proto::TrustConfig::CURRENT_SCHEMA_VERSION`).
    let trust_json = serde_json::json!({
        "schemaVersion": 1,
        "ciReleaseKey": {
            "current": { "algorithm": "ed25519", "public": public_b64 },
            "previous": null,
            "rejectBefore": null,
        },
        "cacheKeys": [],
        "orgRootKey": null,
    });
    let trust = write_pem(dir, "trust.json", &trust_json.to_string());

    (artifact, signature_path, trust)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_with_signed_fleet(
    dir: &TempDir,
    artifact: PathBuf,
    signature: PathBuf,
    trust: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    ca: PathBuf,
    db_path: PathBuf,
    port: u16,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let observed = write_pem(
        dir,
        "observed.json",
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let args = server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: Some(ca),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        // Far-future window — `signedAt` 2026-04-26 must remain valid.
        // The test clock is real wall-clock; freshness is per-channel
        // (60 minutes) but verify_artifact also takes the global window.
        freshness_window: Duration::from_secs(86400 * 365 * 5),
        confirm_deadline_secs: 120,
        db_path: Some(db_path),
        ..Default::default()
    };
    let handle = tokio::spawn(server::serve(args));
    // Give the prime path time to verify the artifact + write the
    // snapshot before any checkin lands.
    sleep(Duration::from_millis(400)).await;
    assert!(!handle.is_finished(), "server task exited prematurely");
    handle
}

const DECLARED_CLOSURE: &str = "decl0001-nixos-system-test-host-26.05";
const CI_COMMIT: &str = "abc12345deadbeefcafebabe";

fn checkin_request(current: &str) -> CheckinRequest {
    CheckinRequest {
        hostname: "test-host".to_string(),
        agent_version: "test".to_string(),
        current_generation: GenerationRef {
            closure_hash: current.to_string(),
            channel_ref: None,
            boot_id: "00000000-0000-0000-0000-000000000000".to_string(),
        },
        pending_generation: None,
        last_evaluated_target: None,
        last_fetch_outcome: Some(FetchOutcome {
            result: FetchResult::Ok,
            error: None,
        }),
        uptime_secs: None,
        last_confirmed_at: None,
    }
}

/// End-to-end smoke test for the dispatch wiring. Subsumes a
/// standalone "dispatch issues target when diverged" test: the first
/// checkin asserts both that the response carries a populated target
/// (with the expected closure + rollout-id derived from the signed
/// fleet's ciCommit) and that the CP wrote a `host_dispatch_state` row.
/// The second checkin asserts the in-flight gate suppresses re-
/// dispatch and no second row is written.
#[tokio::test]
async fn dispatch_end_to_end_signed_fleet_then_idempotent() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (artifact, signature, trust) = write_signed_fleet(&dir, DECLARED_CLOSURE, CI_COMMIT);
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, "test-host");
    let db_path = dir.path().join("state.db");
    let port = pick_free_port().await;

    let handle = spawn_with_signed_fleet(
        &dir,
        artifact,
        signature,
        trust,
        server_cert,
        server_key,
        ca.clone(),
        db_path.clone(),
        port,
    )
    .await;

    let client = build_mtls_client(&ca, &client_cert, &client_key);

    // First checkin: divergent generation → CP issues a target and
    // UPSERTs a host_dispatch_state row.
    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/checkin"))
        .json(&checkin_request("running-system-old"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: CheckinResponse = resp.json().await.unwrap();
    let target = body.target.expect("first checkin should dispatch a target");
    assert_eq!(target.closure_hash, DECLARED_CLOSURE);
    // rolloutId is now a 64-char hex content hash (sha256 over the
    // canonical bytes of the projected RolloutManifest); channel_ref
    // mirrors it on the wire. Exact value depends on every input to
    // the projection — assert shape, not value.
    assert_eq!(target.channel_ref.len(), 64);
    assert!(
        target
            .channel_ref
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "channel_ref must be hex lowercase: {}",
        target.channel_ref,
    );
    assert_eq!(target.rollout_id.as_deref(), Some(target.channel_ref.as_str()));

    // Second checkin (same divergent state): pending row already
    // exists → CP must NOT dispatch again.
    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/checkin"))
        .json(&checkin_request("running-system-old"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: CheckinResponse = resp.json().await.unwrap();
    assert!(
        body.target.is_none(),
        "second checkin while pending: target must be null",
    );

    // DB has exactly one row for this host — confirms both that the
    // first checkin's side effect landed and that the second checkin
    // didn't insert a duplicate.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM host_dispatch_state WHERE hostname = ?1",
            rusqlite::params!["test-host"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "expected exactly one host_dispatch_state row");

    handle.abort();
}
