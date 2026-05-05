//! `/metrics` integration test against a live CP. Exercises the full
//! scrape pipeline (mTLS → fleet_state_view → record_fleet_metrics →
//! PrometheusHandle::render) plus the counter-increment path triggered
//! by `/v1/agent/report`.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use common::{
    build_mtls_client, install_crypto_provider_once, mint_ca_and_certs, pick_free_port,
    wait_for_listener_ready, write_bytes, write_pem,
};
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::server;
use nixfleet_proto::agent_wire::{ReportEvent, ReportRequest};
use rand::rngs::OsRng;
use tempfile::TempDir;

const HOST: &str = "metrics-test-host";
const CHANNEL: &str = "metrics-test-channel";
const DECLARED_CLOSURE: &str = "decl-metrics-1234";
const CI_COMMIT: &str = "ffcafe000000beef1111";

fn build_fleet_resolved_json() -> (String, Vec<u8>) {
    let signed_at = "2026-05-05T00:00:00Z";
    let json = serde_json::json!({
        "schemaVersion": 1,
        "hosts": {
            HOST: {
                "system": "x86_64-linux",
                "tags": [],
                "channel": CHANNEL,
                "closureHash": DECLARED_CLOSURE,
                "pubkey": null,
            }
        },
        "channels": {
            CHANNEL: {
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
            "ciCommit": CI_COMMIT,
            "signatureAlgorithm": "ed25519",
        },
    });
    let raw = serde_json::to_string(&json).unwrap();
    let canonical = nixfleet_canonicalize::canonicalize(&raw).unwrap();
    (raw, canonical.into_bytes())
}

fn write_signed_fleet(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key());
    let (raw_json, canonical_bytes) = build_fleet_resolved_json();
    let signature = signing_key.sign(&canonical_bytes);
    let artifact = write_pem(dir, "fleet.resolved.json", &raw_json);
    let signature_path = write_bytes(dir, "fleet.resolved.json.sig", &signature.to_bytes());
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
async fn spawn_signed(
    dir: &TempDir,
    artifact: PathBuf,
    signature: PathBuf,
    trust: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    ca: PathBuf,
    port: u16,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let observed = write_pem(
        dir,
        "observed.json",
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let db_path = dir.path().join("cp.db");
    let args = server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: Some(ca),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        freshness_window: Duration::from_secs(86400 * 365 * 5),
        confirm_deadline_secs: 120,
        db_path: Some(db_path),
        ..Default::default()
    };
    let handle = tokio::spawn(server::serve(args));
    wait_for_listener_ready(port, &handle).await;
    handle
}

#[tokio::test]
async fn metrics_endpoint_returns_expected_gauges_and_counters() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) = mint_ca_and_certs(&dir, HOST);
    let (artifact, signature, trust) = write_signed_fleet(&dir);
    let port = pick_free_port().await;

    let server_handle = spawn_signed(
        &dir,
        artifact,
        signature,
        trust,
        server_cert,
        server_key,
        ca.clone(),
        port,
    )
    .await;
    let client = build_mtls_client(&ca, &client_cert, &client_key);
    let metrics_url = format!("https://localhost:{port}/metrics");

    // ---- 1. Static + fleet-snapshot metrics present at startup ----
    let resp = client.get(&metrics_url).send().await.unwrap();
    assert_eq!(resp.status(), 200, "/metrics returned {}", resp.status());
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("");
    assert!(
        ct.starts_with("text/plain"),
        "unexpected content-type: {ct}"
    );
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("nixfleet_cp_build_info"),
        "missing build_info gauge:\n{body}"
    );
    assert!(
        body.contains(&format!(
            "nixfleet_channel_freshness_window_minutes{{channel=\"{CHANNEL}\"}}"
        )),
        "missing channel freshness window gauge:\n{body}"
    );
    assert!(
        body.contains("nixfleet_fleet_signed_age_seconds"),
        "missing fleet signed-age gauge:\n{body}"
    );
    assert!(
        body.contains(&format!(
            "nixfleet_host_converged{{channel=\"{CHANNEL}\",host=\"{HOST}\"}}"
        )) || body.contains(&format!(
            "nixfleet_host_converged{{host=\"{HOST}\",channel=\"{CHANNEL}\"}}"
        )),
        "missing host_converged gauge with both labels:\n{body}"
    );

    // ---- 2. Cardinality discipline: forbidden labels never appear ----
    for forbidden in [
        "closure_hash=",
        "rollout_id=",
        "evidence_snippet=",
        "framework_articles=",
        DECLARED_CLOSURE,
    ] {
        assert!(
            !body.contains(forbidden),
            "forbidden label/value '{forbidden}' leaked into /metrics:\n{body}"
        );
    }

    // ---- 3. ComplianceFailure report increments counter + flips gauge ----
    let event_id_payload = ReportRequest {
        hostname: HOST.into(),
        agent_version: "test".into(),
        occurred_at: Utc::now(),
        rollout: None,
        event: ReportEvent::ComplianceFailure {
            control_id: "TEST-CONTROL-A".into(),
            status: "fail".into(),
            framework_articles: vec!["NIS2-Art21".into()],
            evidence_snippet: None,
            evidence_collected_at: Utc::now(),
            signature: None,
        },
    };
    let report_url = format!("https://localhost:{port}/v1/agent/report");
    let resp = client
        .post(&report_url)
        .json(&event_id_payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "POST /v1/agent/report failed: {}",
        resp.status()
    );

    // Re-scrape — counter and outstanding gauge should reflect the event.
    let body2 = client
        .get(&metrics_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body2.contains("nixfleet_compliance_failure_events_total"),
        "missing compliance_failure counter after report:\n{body2}"
    );
    assert!(
        body2.contains("control_id=\"TEST-CONTROL-A\""),
        "missing control_id label after report:\n{body2}"
    );
    assert!(
        body2.contains("nixfleet_host_outstanding_compliance_failures"),
        "missing outstanding gauge after report:\n{body2}"
    );

    // Counter increment is monotonic — second scrape's value strictly
    // exceeds first scrape's (zero or absent before the report posted).
    let count_v1 = scrape_counter(&body, "nixfleet_compliance_failure_events_total");
    let count_v2 = scrape_counter(&body2, "nixfleet_compliance_failure_events_total");
    assert!(
        count_v2 > count_v1,
        "compliance counter did not increment: v1={count_v1} v2={count_v2}"
    );

    server_handle.abort();
}

#[tokio::test]
async fn metrics_returns_503_when_fleet_snapshot_not_primed() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca, server_cert, server_key, client_cert, client_key) = mint_ca_and_certs(&dir, HOST);

    // Empty / unsigned inputs — verify_artifact fails, verified_fleet stays None.
    let artifact = write_pem(&dir, "fleet.resolved.json", "{}");
    let signature = write_pem(&dir, "fleet.resolved.json.sig", "");
    let trust = write_pem(
        &dir,
        "trust.json",
        r#"{"ciReleaseKey":{"current":[],"rejectBefore":null}}"#,
    );
    let observed = write_pem(
        &dir,
        "observed.json",
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    let port = pick_free_port().await;

    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let args = server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: Some(ca.clone()),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        confirm_deadline_secs: 120,
        ..Default::default()
    };
    let handle = tokio::spawn(server::serve(args));
    wait_for_listener_ready(port, &handle).await;

    let client = build_mtls_client(&ca, &client_cert, &client_key);
    let resp = client
        .get(format!("https://localhost:{port}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "expected 503 with un-primed snapshot, got {}",
        resp.status()
    );

    handle.abort();
}

/// Sum every line that starts with `name{` (any label set) — counter
/// values are floats in Prometheus text format. Returns 0.0 when the
/// metric is absent so the "first scrape" baseline reads as zero.
fn scrape_counter(body: &str, name: &str) -> f64 {
    body.lines()
        .filter(|line| line.starts_with(name))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, v)| v.parse::<f64>().ok())
        .sum()
}
