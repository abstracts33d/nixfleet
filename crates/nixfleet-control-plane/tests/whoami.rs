//! `/v1/whoami` integration test.
//!
//! Mints a synthetic fleet CA + server cert + client cert in-test
//! with rcgen, spins up `serve` with the CA wired as `--client-ca`
//! (mTLS-required mode), hits `/v1/whoami` with the client cert and
//! asserts the verified CN matches what we put in the cert.
//!
//! Also covers the negative case: same server, but a request
//! without a client cert is rejected at the TLS handshake.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{install_crypto_provider_once, mint_ca_and_certs, pick_free_port, write_pem};
use nixfleet_control_plane::server;
use reqwest::{Certificate as ReqwestCert, Identity};
use serde::Deserialize;
use tempfile::TempDir;
use tokio::time::sleep;

#[derive(Debug, Deserialize)]
struct WhoamiBody {
    cn: String,
    #[serde(rename = "issuedAt")]
    #[allow(dead_code)]
    issued_at: String,
}

/// Minimal stub inputs the reconcile loop expects to find. See the
/// `/healthz` test for the rationale: `/v1/whoami` doesn't depend on
/// the reconcile loop, but the serve loop spawns it regardless.
fn write_phase2_input_stubs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let artifact = write_pem(dir, "fleet.resolved.json", "{}");
    let signature = write_pem(dir, "fleet.resolved.json.sig", "");
    let trust = write_pem(
        dir,
        "trust.json",
        r#"{"ciReleaseKey":{"current":[],"rejectBefore":null}}"#,
    );
    let observed = write_pem(
        dir,
        "observed.json",
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    (artifact, signature, trust, observed)
}

async fn spawn_server(args: server::ServeArgs) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let handle = tokio::spawn(server::serve(args));
    sleep(Duration::from_millis(200)).await;
    assert!(
        !handle.is_finished(),
        "server task exited before tests could run (TLS config error?)"
    );
    handle
}

#[tokio::test]
async fn whoami_returns_verified_cn_when_client_cert_present() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, "test-host");
    let (artifact, signature, trust, observed) = write_phase2_input_stubs(&dir);

    let port = pick_free_port().await;
    let listen = format!("127.0.0.1:{port}").parse().unwrap();

    let server_handle = spawn_server(server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: Some(ca.clone()),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        freshness_window: Duration::from_secs(86400),
        confirm_deadline_secs: 120,
        channel_refs: None,
        revocations: None,
        db_path: None,
        closure_upstream: None,
        rollouts_dir: None,
        rollouts_source: None,
        fleet_ca_cert: None,
        fleet_ca_key: None,
        audit_log_path: None,
    })
    .await;

    // Build reqwest client with our client cert + key as Identity.
    let mut client_pem_bytes = std::fs::read(&client_cert).unwrap();
    client_pem_bytes.extend_from_slice(&std::fs::read(&client_key).unwrap());
    let identity = Identity::from_pem(&client_pem_bytes).unwrap();
    let ca_pem = std::fs::read(&ca).unwrap();
    let ca_cert = ReqwestCert::from_pem(&ca_pem).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_cert)
        .identity(identity)
        .build()
        .unwrap();

    let url = format!("https://localhost:{port}/v1/whoami");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: WhoamiBody = resp.json().await.unwrap();
    assert_eq!(body.cn, "test-host");

    server_handle.abort();
}

#[tokio::test]
async fn whoami_rejects_request_without_client_cert() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, _client_cert, _client_key) =
        mint_ca_and_certs(&dir, "test-host");
    let (artifact, signature, trust, observed) = write_phase2_input_stubs(&dir);

    let port = pick_free_port().await;
    let listen = format!("127.0.0.1:{port}").parse().unwrap();

    let server_handle = spawn_server(server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: Some(ca.clone()),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        freshness_window: Duration::from_secs(86400),
        confirm_deadline_secs: 120,
        channel_refs: None,
        revocations: None,
        db_path: None,
        closure_upstream: None,
        rollouts_dir: None,
        rollouts_source: None,
        fleet_ca_cert: None,
        fleet_ca_key: None,
        audit_log_path: None,
    })
    .await;

    // Same client config but NO identity — no client cert presented.
    // Server's WebPkiClientVerifier rejects the handshake; reqwest
    // surfaces that as a connect error.
    let ca_pem = std::fs::read(&ca).unwrap();
    let ca_cert = ReqwestCert::from_pem(&ca_pem).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_cert)
        .build()
        .unwrap();

    let url = format!("https://localhost:{port}/v1/whoami");
    let result = client.get(&url).send().await;
    assert!(
        result.is_err(),
        "expected TLS handshake failure when client presents no cert, got: {result:?}"
    );

    server_handle.abort();
}
