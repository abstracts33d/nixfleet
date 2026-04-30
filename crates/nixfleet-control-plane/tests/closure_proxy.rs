//! `/v1/agent/closure/{hash}` integration tests.
//!
//! Coverage:
//! - 501 Not Implemented when no upstream is configured.
//! - 200 forward when upstream is reachable: stub HTTP server
//!   returns a synthetic narinfo body, CP forwards it verbatim.
//! - 502 Bad Gateway when upstream is unreachable.
//!
//! The stub upstream is a tiny tokio TcpListener that handles one
//! connection per test — minimal moving parts, no wiremock dep.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use common::{
    build_mtls_client, install_crypto_provider_once, mint_ca_and_certs, pick_free_port, write_pem,
};
use nixfleet_control_plane::server;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::sleep;

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

async fn spawn_cp(
    dir: &TempDir,
    server_cert: PathBuf,
    server_key: PathBuf,
    ca: PathBuf,
    cp_port: u16,
    closure_upstream: Option<String>,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (artifact, signature, trust, observed) = write_phase2_input_stubs(dir);
    let args = server::ServeArgs {
        listen: format!("127.0.0.1:{cp_port}").parse().unwrap(),
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
        freshness_window: Duration::from_secs(86400),
        confirm_deadline_secs: 120,
        channel_refs: None,
        revocations: None,
        db_path: None,
        closure_upstream,
        rollouts_dir: None,
        rollouts_source: None,
    };
    let handle = tokio::spawn(server::serve(args));
    sleep(Duration::from_millis(300)).await;
    assert!(!handle.is_finished(), "server task exited prematurely");
    handle
}

/// Tiny stub HTTP server. Handles ONE connection: reads the request
/// (drains until \r\n\r\n) and replies with `body` as the HTTP body.
async fn stub_http_once(addr: SocketAddr, body: &'static str) -> tokio::task::JoinHandle<()> {
    let listener = TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        // Drain — we don't bother parsing.
        let _ = &buf[..n];
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/x-nix-narinfo\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
    })
}

#[tokio::test]
async fn closure_proxy_returns_501_when_upstream_unset() {
    install_crypto_provider_once();
    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, "test-host");

    let cp_port = pick_free_port().await;
    let handle = spawn_cp(&dir, server_cert, server_key, ca.clone(), cp_port, None).await;

    let client = build_mtls_client(&ca, &client_cert, &client_key);
    let resp = client
        .get(format!(
            "https://localhost:{cp_port}/v1/agent/closure/abc123"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("closure proxy not configured"),
        "body: {body}"
    );

    handle.abort();
}

#[tokio::test]
async fn closure_proxy_forwards_to_upstream() {
    install_crypto_provider_once();
    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, "test-host");

    // Stub HTTP upstream. Must bind before spawning CP so the CP can
    // resolve the URL at startup.
    let upstream_port = pick_free_port().await;
    let upstream_addr: SocketAddr = format!("127.0.0.1:{upstream_port}").parse().unwrap();
    let stub_body = "StorePath: /nix/store/abc123-test\nURL: nar/abc.nar.zst\n";
    let stub = stub_http_once(upstream_addr, stub_body).await;

    let cp_port = pick_free_port().await;
    let handle = spawn_cp(
        &dir,
        server_cert,
        server_key,
        ca.clone(),
        cp_port,
        Some(format!("http://127.0.0.1:{upstream_port}")),
    )
    .await;

    let client = build_mtls_client(&ca, &client_cert, &client_key);
    let resp = client
        .get(format!("https://localhost:{cp_port}/v1/agent/closure/abc123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, stub_body);

    stub.abort();
    handle.abort();
}

#[tokio::test]
async fn closure_proxy_returns_502_when_upstream_unreachable() {
    install_crypto_provider_once();
    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, "test-host");

    // Pick a port and DON'T bind anything to it — guaranteed
    // connection refused. Note: there's a small race where another
    // process could bind it before our test runs. Acceptable for a
    // local test.
    let dead_port = pick_free_port().await;

    let cp_port = pick_free_port().await;
    let handle = spawn_cp(
        &dir,
        server_cert,
        server_key,
        ca.clone(),
        cp_port,
        Some(format!("http://127.0.0.1:{dead_port}")),
    )
    .await;

    let client = build_mtls_client(&ca, &client_cert, &client_key);
    let resp = client
        .get(format!("https://localhost:{cp_port}/v1/agent/closure/abc123"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);

    handle.abort();
}
