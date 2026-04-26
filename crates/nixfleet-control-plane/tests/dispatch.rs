//! Phase 4 dispatch-loop integration test.
//!
//! Drives the full path: a real ed25519-signed `fleet.resolved.json`
//! is verified at server boot (priming `AppState.verified_fleet`);
//! an agent checks in with a current generation that diverges from
//! the declared closure; the response carries a populated `target`;
//! the DB has a matching `pending_confirms` row.
//!
//! The same agent checking in twice in quick succession must NOT
//! create a second pending row (idempotency: `pending_confirm_exists`
//! gate). A converged agent gets `target: null` and no DB row.
//!
//! Cert-minting + spawn helpers duplicate `confirm.rs` because cargo
//! integration tests can't share a `mod common`.

use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::{db::Db, server};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, FetchOutcome, FetchResult, GenerationRef,
};
use rand::rngs::OsRng;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use reqwest::{Certificate as ReqwestCert, Identity};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::sleep;

fn install_crypto_provider_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        // Cheap: lets RUST_LOG=info|debug surface dispatch / reconcile
        // tracing during integration tests when triaging.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .try_init();
    });
}

async fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_pem(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_bytes(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// Build a minimal valid `fleet.resolved.json` declaring `krach`'s
/// channel + target closure. Returns the canonical bytes (sign these)
/// and the original JSON string (write to disk; the server re-
/// canonicalizes on read).
fn build_fleet_resolved_json(declared_closure: &str, ci_commit: &str) -> (String, Vec<u8>) {
    // Hand-rolled JSON to keep the test independent of the proto
    // crate's serde shape. Field order matches what Stream B emits;
    // canonicalization will resort it anyway, so order is cosmetic.
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
        "atticCacheKey": null,
        "orgRootKey": null,
    });
    let trust = write_pem(dir, "trust.json", &trust_json.to_string());

    (artifact, signature_path, trust)
}

fn mint_ca_and_certs(
    dir: &TempDir,
    client_cn: &str,
) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "test-fleet-ca");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert: Certificate = ca_params.self_signed(&ca_key).unwrap();

    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "test-cp-server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key).unwrap();

    let mut client_params = CertificateParams::default();
    client_params
        .distinguished_name
        .push(DnType::CommonName, client_cn);
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_key = KeyPair::generate().unwrap();
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    (
        write_pem(dir, "ca.pem", &ca_cert.pem()),
        write_pem(dir, "server.pem", &server_cert.pem()),
        write_pem(dir, "server.key", &server_key.serialize_pem()),
        write_pem(dir, "client.pem", &client_cert.pem()),
        write_pem(dir, "client.key", &client_key.serialize_pem()),
    )
}

fn build_mtls_client(ca: &PathBuf, client_cert: &PathBuf, client_key: &PathBuf) -> reqwest::Client {
    let mut pem = std::fs::read(client_cert).unwrap();
    pem.extend_from_slice(&std::fs::read(client_key).unwrap());
    let identity = Identity::from_pem(&pem).unwrap();
    let ca_pem = std::fs::read(ca).unwrap();
    let ca_cert = ReqwestCert::from_pem(&ca_pem).unwrap();
    reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_cert)
        .identity(identity)
        .build()
        .unwrap()
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
        fleet_ca_cert: None,
        fleet_ca_key: None,
        audit_log_path: None,
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        // Far-future window — `signedAt` 2026-04-26 must remain valid.
        // The test clock is real wall-clock; freshness is per-channel
        // (60 minutes) but verify_artifact also takes the global window.
        freshness_window: Duration::from_secs(86400 * 365 * 5),
        forgejo: None,
        db_path: Some(db_path),
        closure_upstream: None,
    };
    let handle = tokio::spawn(server::serve(args));
    // Give the prime path time to verify the artifact + write the
    // snapshot before any checkin lands.
    sleep(Duration::from_millis(400)).await;
    assert!(!handle.is_finished(), "server task exited prematurely");
    handle
}

const DECLARED_CLOSURE: &str = "decl0001-nixos-system-krach-26.05";
const CI_COMMIT: &str = "abc12345deadbeefcafebabe";

fn checkin_request(current: &str) -> CheckinRequest {
    CheckinRequest {
        hostname: "krach".to_string(),
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
    }
}

#[tokio::test]
async fn dispatch_issues_target_when_diverged() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (artifact, signature, trust) = write_signed_fleet(&dir, DECLARED_CLOSURE, CI_COMMIT);
    let (ca, server_cert, server_key, client_cert, client_key) = mint_ca_and_certs(&dir, "krach");
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
    let resp = client
        .post(&format!("https://localhost:{port}/v1/agent/checkin"))
        .json(&checkin_request("running-system-old"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: CheckinResponse = resp.json().await.unwrap();
    let target = body.target.expect("dispatch should issue a target");
    assert_eq!(target.closure_hash, DECLARED_CLOSURE);
    // Rollout id format: `<channel>@<ci-commit-prefix>` (8 chars).
    assert_eq!(target.channel_ref, "stable@abc12345");

    // Verify the DB has a matching pending_confirms row.
    let db = Db::open(&db_path).unwrap();
    assert!(
        db.pending_confirm_exists("krach").unwrap(),
        "expected a pending_confirms row for krach",
    );

    handle.abort();
}

#[tokio::test]
async fn dispatch_returns_null_when_converged() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (artifact, signature, trust) = write_signed_fleet(&dir, DECLARED_CLOSURE, CI_COMMIT);
    let (ca, server_cert, server_key, client_cert, client_key) = mint_ca_and_certs(&dir, "krach");
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
    let resp = client
        .post(&format!("https://localhost:{port}/v1/agent/checkin"))
        .json(&checkin_request(DECLARED_CLOSURE)) // already converged
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: CheckinResponse = resp.json().await.unwrap();
    assert!(
        body.target.is_none(),
        "converged agent should get target: null",
    );
    let db = Db::open(&db_path).unwrap();
    assert!(
        !db.pending_confirm_exists("krach").unwrap(),
        "no pending_confirms row should have been written",
    );

    handle.abort();
}

#[tokio::test]
async fn dispatch_is_idempotent_across_two_checkins() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (artifact, signature, trust) = write_signed_fleet(&dir, DECLARED_CLOSURE, CI_COMMIT);
    let (ca, server_cert, server_key, client_cert, client_key) = mint_ca_and_certs(&dir, "krach");
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

    // First checkin: target dispatched, row inserted.
    let resp = client
        .post(&format!("https://localhost:{port}/v1/agent/checkin"))
        .json(&checkin_request("running-system-old"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: CheckinResponse = resp.json().await.unwrap();
    assert!(body.target.is_some(), "first checkin should dispatch");

    // Second checkin: pending row already exists → CP should NOT
    // dispatch again (no second row, no target in response).
    let resp = client
        .post(&format!("https://localhost:{port}/v1/agent/checkin"))
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

    // DB has exactly one row for this host.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_confirms WHERE hostname = ?1",
            rusqlite::params!["krach"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "expected exactly one pending_confirms row");

    handle.abort();
}
