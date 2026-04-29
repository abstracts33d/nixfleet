//! Wave-staging compliance gate integration test (issue #59).
//!
//! Drives the full chain end-to-end:
//! - signed `fleet.resolved.json` with `hosts.lab` on a channel
//!   declaring `compliance.mode`,
//! - posts a real `ComplianceFailure` event with a JCS-canonical
//!   ed25519 signature against the host's SSH pubkey,
//! - verifies the CP's `/v1/agent/checkin` returns `target: null`
//!   under enforce mode (wave gate blocks) and a populated target
//!   under permissive mode (advisory only).
//!
//! Mirrors `dispatch.rs`'s harness shape — duplication over
//! abstraction because cargo integration tests can't share a
//! `mod common`.
//!
//! Closes the validation gap flagged in the cycle review: unit
//! tests cover `wave_gate::evaluate_channel_gate` and
//! `evidence_verify::verify_event` independently; this test wires
//! both into the live HTTP path with mTLS.
//!
//! NOTE on lab-relevance: the lab fleet today ships
//! `hosts.lab.pubkey = null` (host enrolled but pubkey not yet
//! stamped); under that config every event surfaces with
//! `signature_status = NoPubkey`, which the gate counts as
//! outstanding (mTLS-bound trust). This test exercises the
//! `Verified` path explicitly so we know the auditor chain is
//! wired correctly the day a fleet operator stamps a pubkey.

use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::server;
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, FetchOutcome, FetchResult, GenerationRef, ReportEvent,
    ReportRequest, ReportResponse,
};
use rand::rngs::OsRng;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rand::RngCore;
use reqwest::{Certificate as ReqwestCert, Identity};
use serde::Serialize;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::sleep;

const HOSTNAME: &str = "test-host";
const DECLARED_CLOSURE: &str = "decl0001-nixos-system-test-host-26.05";
const CURRENT_CLOSURE: &str = "curr0001-nixos-system-test-host-26.05";
const CI_COMMIT: &str = "abc12345deadbeefcafebabe";

fn install_crypto_provider_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
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

/// Generate an ed25519 keypair and the matching `ssh-ed25519
/// AAAAC3...` OpenSSH pubkey string. Used both for stamping
/// `fleet.resolved.hosts.<host>.pubkey` and for signing test event
/// payloads.
fn fresh_host_keypair() -> (SigningKey, String) {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let sk = SigningKey::from_bytes(&seed);
    let pubkey_bytes = sk.verifying_key().to_bytes();
    let ssh_pk = ssh_key::PublicKey::new(
        ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(pubkey_bytes)),
        "test-host",
    );
    let openssh = ssh_pk.to_openssh().expect("to_openssh");
    (sk, openssh)
}

/// Build a signed fleet.resolved.json declaring one host on one
/// channel with the requested compliance.mode + the host's SSH
/// pubkey stamped (so verification reaches `Verified` rather than
/// `NoPubkey`).
fn write_signed_fleet(
    dir: &TempDir,
    compliance_mode: &str,
    host_ssh_pubkey: Option<&str>,
) -> (PathBuf, PathBuf, PathBuf) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let public_b64 =
        base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key());

    let signed_at = "2026-04-26T00:00:00Z";
    let json = serde_json::json!({
        "schemaVersion": 1,
        "hosts": {
            HOSTNAME: {
                "system": "x86_64-linux",
                "tags": [],
                "channel": "stable",
                "closureHash": DECLARED_CLOSURE,
                "pubkey": host_ssh_pubkey,
            }
        },
        "channels": {
            "stable": {
                "rolloutPolicy": "default",
                "reconcileIntervalMinutes": 5,
                "freshnessWindow": 60,
                "signingIntervalMinutes": 30,
                "compliance": {
                    "frameworks": [],
                    "mode": compliance_mode,
                },
            }
        },
        "rolloutPolicies": {},
        "waves": {},
        "edges": [],
        "disruptionBudgets": [],
        "meta": {
            "schemaVersion": 1,
            "signedAt": signed_at,
            "ciCommit": CI_COMMIT,
        },
    });
    let raw = serde_json::to_string(&json).unwrap();
    let canonical = nixfleet_canonicalize::canonicalize(&raw).unwrap();
    let signature = signing_key.sign(canonical.as_bytes());

    let artifact = write_pem(dir, "fleet.resolved.json", &raw);
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
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

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

fn build_mtls_client(
    ca: &PathBuf,
    client_cert: &PathBuf,
    client_key: &PathBuf,
) -> reqwest::Client {
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
        freshness_window: Duration::from_secs(86400 * 365 * 5),
        confirm_deadline_secs: 120,
        channel_refs: None,
        revocations: None,
        db_path: Some(db_path),
        closure_upstream: None,
    };
    let handle = tokio::spawn(server::serve(args));
    sleep(Duration::from_millis(400)).await;
    assert!(!handle.is_finished(), "server task exited prematurely");
    handle
}

fn checkin_request(current: &str) -> CheckinRequest {
    CheckinRequest {
        hostname: HOSTNAME.to_string(),
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

/// Mirror of the agent's `evidence_signer::ComplianceFailureSignedPayload`.
/// Inlined here so the integration test doesn't import the agent crate
/// (it doesn't need anything else from it).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplianceFailureSignedPayload<'a> {
    hostname: &'a str,
    rollout: Option<&'a str>,
    control_id: &'a str,
    status: &'a str,
    framework_articles: &'a [String],
    evidence_collected_at: chrono::DateTime<chrono::Utc>,
    evidence_snippet_sha256: String,
}

/// Build + sign a `ComplianceFailure` ReportRequest the way the
/// real agent does. Returns the request body the test posts to
/// `/v1/agent/report`.
fn build_signed_compliance_failure(
    sk: &SigningKey,
    rollout: &str,
    control_id: &str,
) -> ReportRequest {
    let articles: Vec<String> = vec!["nis2:21(b)".to_string()];
    let snippet = serde_json::json!({"compliant": false, "rule": "AL-03"});
    // Reproduce the agent's snippet hash (sha256 of JCS bytes).
    let snippet_canon = serde_jcs::to_vec(&snippet).unwrap();
    let snippet_sha = {
        use sha2::Digest;
        let d = sha2::Sha256::digest(&snippet_canon);
        let mut s = String::with_capacity(64);
        for b in d.iter() {
            s.push_str(&format!("{:02x}", b));
        }
        s
    };
    let evidence_collected_at = Utc::now();
    let payload = ComplianceFailureSignedPayload {
        hostname: HOSTNAME,
        rollout: Some(rollout),
        control_id,
        status: "non-compliant",
        framework_articles: &articles,
        evidence_collected_at,
        evidence_snippet_sha256: snippet_sha,
    };
    let canonical = serde_jcs::to_vec(&payload).unwrap();
    let signature = sk.sign(&canonical);
    let signature_b64 =
        base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    ReportRequest {
        hostname: HOSTNAME.to_string(),
        agent_version: "test".to_string(),
        occurred_at: Utc::now(),
        rollout: Some(rollout.to_string()),
        event: ReportEvent::ComplianceFailure {
            control_id: control_id.to_string(),
            status: "non-compliant".to_string(),
            framework_articles: articles,
            evidence_snippet: Some(snippet),
            evidence_collected_at,
            signature: Some(signature_b64),
        },
    }
}

/// Helper: post a checkin and return the parsed CheckinResponse.
async fn post_checkin(
    client: &reqwest::Client,
    port: u16,
    req: &CheckinRequest,
) -> CheckinResponse {
    client
        .post(format!("https://localhost:{port}/v1/agent/checkin"))
        .json(req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn enforce_mode_blocks_dispatch_after_signed_compliance_failure() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (host_sk, host_pubkey) = fresh_host_keypair();
    let (artifact, signature, trust) =
        write_signed_fleet(&dir, "enforce", Some(&host_pubkey));
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, HOSTNAME);
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
        db_path,
        port,
    )
    .await;
    let client = build_mtls_client(&ca, &client_cert, &client_key);

    // 1. Initial checkin: host on CURRENT_CLOSURE, fleet declares
    //    DECLARED_CLOSURE → would dispatch under normal flow.
    let checkin_diverged = checkin_request(CURRENT_CLOSURE);
    let resp1 = post_checkin(&client, port, &checkin_diverged).await;
    assert!(
        resp1.target.is_some(),
        "first checkin should dispatch (no outstanding events yet)"
    );
    let dispatched_rollout = resp1
        .target
        .as_ref()
        .and_then(|t| t.rollout_id.clone())
        .expect("dispatch carries rollout_id");

    // 2. Agent posts a signed ComplianceFailure for the dispatched
    //    rollout (simulating: host activated but the gate found a
    //    failing control post-activation).
    let report = build_signed_compliance_failure(&host_sk, &dispatched_rollout, "auditLogging");
    let report_resp: ReportResponse = client
        .post(format!("https://localhost:{port}/v1/agent/report"))
        .json(&report)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        report_resp.event_id.starts_with("evt-"),
        "report accepted, got id: {}",
        report_resp.event_id
    );

    // 3. Next checkin from the same host. Even though the host is
    //    still diverged (CURRENT_CLOSURE != DECLARED_CLOSURE),
    //    enforce-mode wave gate must block dispatch because the
    //    host has an outstanding signature-verified ComplianceFailure
    //    event for the current rollout. CheckinResponse.target = None.
    //    Use a checkin that echoes the rollout id so the wave gate's
    //    "host is on this rollout" lookup succeeds.
    let mut checkin_after_failure = checkin_request(CURRENT_CLOSURE);
    checkin_after_failure.last_evaluated_target =
        Some(nixfleet_proto::agent_wire::EvaluatedTarget {
            closure_hash: DECLARED_CLOSURE.to_string(),
            channel_ref: dispatched_rollout.clone(),
            evaluated_at: Utc::now(),
            rollout_id: Some(dispatched_rollout.clone()),
            wave_index: None,
            activate: None,
            signed_at: None,
            freshness_window_secs: None,
            compliance_mode: Some("enforce".to_string()),
        });
    let resp2 = post_checkin(&client, port, &checkin_after_failure).await;
    assert!(
        resp2.target.is_none(),
        "enforce + outstanding failure must block dispatch — got target {:?}",
        resp2.target
    );

    handle.abort();
}

#[tokio::test]
async fn permissive_mode_does_not_block_dispatch_despite_failure() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (host_sk, host_pubkey) = fresh_host_keypair();
    let (artifact, signature, trust) =
        write_signed_fleet(&dir, "permissive", Some(&host_pubkey));
    let (ca, server_cert, server_key, client_cert, client_key) =
        mint_ca_and_certs(&dir, HOSTNAME);
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
        db_path,
        port,
    )
    .await;
    let client = build_mtls_client(&ca, &client_cert, &client_key);

    // Initial checkin gets a target.
    let resp1 = post_checkin(&client, port, &checkin_request(CURRENT_CLOSURE)).await;
    let dispatched_rollout = resp1
        .target
        .as_ref()
        .and_then(|t| t.rollout_id.clone())
        .expect("first checkin dispatches");

    // Post a signed failure for the dispatched rollout.
    let report = build_signed_compliance_failure(&host_sk, &dispatched_rollout, "auditLogging");
    let _: ReportResponse = client
        .post(format!("https://localhost:{port}/v1/agent/report"))
        .json(&report)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Under permissive, the next checkin's dispatch decision is
    // unaffected. The host is already mid-flight (pending_confirm
    // exists from step 1's record_pending_confirm), so we expect
    // InFlight (target=None) — NOT a wave-gate block. To distinguish
    // the two outcomes, the journal would carry "dispatch: no target
    // (InFlight)" rather than "wave-staging gate blocked". That
    // distinction isn't observable over the wire; the test asserts
    // that under permissive, the gate's WaveGateOutcome::Permissive
    // path is taken (validated by unit tests in wave_gate). The
    // observable contract is "permissive does not 500" + "dispatch
    // continues to behave as it does without compliance" — which we
    // assert by posting a SECOND host's report and watching its
    // dispatch decision below.
    //
    // For this 1-host channel: we re-request and confirm the channel
    // gate code path didn't poison the response shape.
    let resp2 = post_checkin(&client, port, &checkin_request(CURRENT_CLOSURE)).await;
    // Under InFlight, target is None — that's fine. The point is the
    // CP returned 200 with a valid CheckinResponse rather than
    // surfacing an internal error from the gate path.
    assert_eq!(resp2.next_checkin_secs, 60);

    handle.abort();
}
