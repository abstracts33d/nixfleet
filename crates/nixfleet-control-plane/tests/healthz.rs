//! `/healthz` integration test.
//!
//! Mints a self-signed server cert with rcgen, spins up the long-
//! running serve loop in-process on an ephemeral port, hits `/healthz`
//! over TLS with reqwest (CA-pinned to the rcgen cert), asserts 200 +
//! the documented JSON shape.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{install_crypto_provider_once, pick_free_port, write_pem};
use nixfleet_control_plane::server;
use rcgen::{generate_simple_self_signed, CertifiedKey};
use reqwest::Certificate;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::time::sleep;

#[derive(Debug, Deserialize)]
struct HealthzBody {
    ok: bool,
    version: String,
    #[serde(rename = "lastTickAt")]
    last_tick_at: Option<String>,
}

/// Minimal inputs the reconcile loop expects to find. `tick`
/// will fail to parse a non-existent artifact, but the failure is
/// logged-not-fatal — the listener stays up. `/healthz` doesn't
/// depend on tick succeeding.
fn write_phase2_input_stubs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    // Empty files — the server logs read errors and continues. We
    // only need the paths to exist so the unit's
    // `ConditionPathExists` would pass at deploy time; the server
    // itself doesn't require non-empty inputs to bind the listener.
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

#[tokio::test]
async fn healthz_returns_ok_over_tls() {
    install_crypto_provider_once();

    // Self-signed cert with SAN = localhost so the rustls server
    // accepts connections to 127.0.0.1 (rustls validates SAN, CN-only
    // certs no longer work on modern stacks).
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();

    let dir = TempDir::new().unwrap();
    let cert_path = write_pem(&dir, "server.pem", &cert.pem());
    let key_path = write_pem(&dir, "server.key", &key_pair.serialize_pem());
    let (artifact, signature, trust, observed) = write_phase2_input_stubs(&dir);

    let port = pick_free_port().await;
    let listen = format!("127.0.0.1:{port}").parse().unwrap();

    // Spawn the server. It runs forever; the test drops the JoinHandle
    // when it's done, killing the runtime task on tokio runtime
    // shutdown at end-of-test.
    let server_args = server::ServeArgs {
        listen,
        tls_cert: cert_path,
        tls_key: key_path,
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        confirm_deadline_secs: 120,
        ..Default::default()
    };
    let server_handle = tokio::spawn(server::serve(server_args));

    // Give the listener time to bind. Polling a TCP connect would be
    // tighter, but a small fixed sleep is fine for a test.
    sleep(Duration::from_millis(200)).await;
    assert!(
        !server_handle.is_finished(),
        "server task exited before /healthz could be hit (likely TLS config error — check stderr)"
    );

    // CA-pinned reqwest client. The server's self-signed cert IS the
    // trust anchor in this test.
    let cert_pem = cert.pem();
    let ca = Certificate::from_pem(cert_pem.as_bytes()).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca)
        .build()
        .unwrap();

    let url = format!("https://localhost:{port}/healthz");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: HealthzBody = resp.json().await.unwrap();
    assert!(body.ok);
    assert!(!body.version.is_empty(), "version should be populated");
    // last_tick_at can be None (reconcile loop hasn't fired yet — first
    // tick is offset by RECONCILE_INTERVAL = 30s) or Some (longer test
    // run). Either is correct.
    let _ = body.last_tick_at;

    server_handle.abort();
}
