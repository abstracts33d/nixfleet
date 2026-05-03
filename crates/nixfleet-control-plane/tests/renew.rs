//! Integration tests for `/v1/agent/renew`.

mod common;

use std::path::PathBuf;

use chrono::Utc;
use common::{install_crypto_provider_once, pick_free_port, wait_for_listener_ready};
use nixfleet_control_plane::{db::Db, server};
use nixfleet_proto::enroll_wire::{RenewRequest, RenewResponse};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CertificateSigningRequest, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use reqwest::Identity;
use tempfile::TempDir;

fn write_pem(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

struct TestPki {
    ca_cert: PathBuf,
    ca_key: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    agent_cert_pem: String,
    agent_key_pem: String,
}

fn mint_pki(dir: &TempDir, agent_cn: &str) -> TestPki {
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

    let mut agent_params = CertificateParams::default();
    agent_params
        .distinguished_name
        .push(DnType::CommonName, agent_cn);
    agent_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let agent_key = KeyPair::generate().unwrap();
    let agent_cert = agent_params
        .signed_by(&agent_key, &ca_cert, &ca_key)
        .unwrap();

    let ca_cert_path = dir.path().join("ca.pem");
    let ca_key_path = dir.path().join("ca.key");
    let server_cert_path = dir.path().join("server.pem");
    let server_key_path = dir.path().join("server.key");
    write_pem(&ca_cert_path, &ca_cert.pem());
    write_pem(&ca_key_path, &ca_key.serialize_pem());
    write_pem(&server_cert_path, &server_cert.pem());
    write_pem(&server_key_path, &server_key.serialize_pem());

    TestPki {
        ca_cert: ca_cert_path,
        ca_key: ca_key_path,
        server_cert: server_cert_path,
        server_key: server_key_path,
        agent_cert_pem: agent_cert.pem(),
        agent_key_pem: agent_key.serialize_pem(),
    }
}

fn write_phase2_input_stubs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let artifact = dir.path().join("fleet.resolved.json");
    write_pem(&artifact, "{}");
    let signature = dir.path().join("fleet.resolved.json.sig");
    write_pem(&signature, "");
    let trust = dir.path().join("trust-stub.json");
    write_pem(
        &trust,
        r#"{"schemaVersion":1,"ciReleaseKey":{"current":null,"previous":null,"rejectBefore":null}}"#,
    );
    let observed = dir.path().join("observed.json");
    write_pem(
        &observed,
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    (artifact, signature, trust, observed)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_server(
    args_dir: &TempDir,
    server_cert: PathBuf,
    server_key: PathBuf,
    client_ca: Option<PathBuf>,
    fleet_ca_cert: Option<PathBuf>,
    fleet_ca_key: Option<PathBuf>,
    db_path: Option<PathBuf>,
    port: u16,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (artifact, signature, trust, observed) = write_phase2_input_stubs(args_dir);
    let audit_log = args_dir.path().join("issuance.log");
    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let args = server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca,
        fleet_ca_cert,
        fleet_ca_key,
        audit_log_path: Some(audit_log),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        confirm_deadline_secs: 120,
        db_path,
        ..Default::default()
    };
    let handle = tokio::spawn(server::serve(args));
    wait_for_listener_ready(port, &handle).await;
    handle
}

fn build_mtls_client(
    ca_pem_path: &std::path::Path,
    agent_pem: &str,
    agent_key_pem: &str,
) -> reqwest::Client {
    let mut combined = agent_pem.as_bytes().to_vec();
    combined.extend_from_slice(agent_key_pem.as_bytes());
    let identity = Identity::from_pem(&combined).unwrap();
    let ca_pem = std::fs::read(ca_pem_path).unwrap();
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
    reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_cert)
        .identity(identity)
        .build()
        .unwrap()
}

fn build_no_cert_client(ca_pem_path: &std::path::Path) -> reqwest::Client {
    let ca_pem = std::fs::read(ca_pem_path).unwrap();
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
    reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_cert)
        .build()
        .unwrap()
}

fn mint_csr(hostname: &str) -> String {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, hostname);
    let csr: CertificateSigningRequest = params.serialize_request(&key).unwrap();
    csr.pem().unwrap()
}

#[tokio::test]
async fn renew_happy_path_signs_fresh_cert() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let pki = mint_pki(&dir, "test-host");
    let port = pick_free_port().await;
    let handle = spawn_server(
        &dir,
        pki.server_cert.clone(),
        pki.server_key.clone(),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_key.clone()),
        None,
        port,
    )
    .await;

    let csr_pem = mint_csr("test-host");
    let req = RenewRequest { csr_pem };
    let client = build_mtls_client(&pki.ca_cert, &pki.agent_cert_pem, &pki.agent_key_pem);

    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/renew"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "expected 200");
    let body: RenewResponse = resp.json().await.unwrap();
    assert!(
        body.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"),
        "renew should return a PEM-encoded cert; got: {}",
        body.cert_pem.chars().take(40).collect::<String>(),
    );
    assert!(
        body.not_after > Utc::now(),
        "not_after must be in the future",
    );

    handle.abort();
}

#[tokio::test]
async fn renew_rejects_request_without_client_cert() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let pki = mint_pki(&dir, "test-host");
    let port = pick_free_port().await;
    let handle = spawn_server(
        &dir,
        pki.server_cert.clone(),
        pki.server_key.clone(),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_key.clone()),
        None,
        port,
    )
    .await;

    let csr_pem = mint_csr("test-host");
    let req = RenewRequest { csr_pem };
    let client = build_no_cert_client(&pki.ca_cert);

    // GOTCHA: TLS-layer rejection or HTTP 401 are both acceptable shapes for "rejected".
    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/renew"))
        .json(&req)
        .send()
        .await;
    match resp {
        Ok(r) => assert_eq!(r.status(), 401, "expected 401, got {}", r.status()),
        Err(_) => {}
    }

    handle.abort();
}

#[tokio::test]
async fn renew_rejects_revoked_cert() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let pki = mint_pki(&dir, "test-host");
    let db_path = dir.path().join("state.db");

    {
        let db = Db::open(&db_path).unwrap();
        db.migrate().unwrap();
        let revoked_before = Utc::now() + chrono::Duration::days(365);
        db.revocations()
            .revoke_cert(
                "test-host",
                revoked_before,
                Some("test"),
                Some("test-operator"),
            )
            .unwrap();
    }

    let port = pick_free_port().await;
    let handle = spawn_server(
        &dir,
        pki.server_cert.clone(),
        pki.server_key.clone(),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_cert.clone()),
        Some(pki.ca_key.clone()),
        Some(db_path),
        port,
    )
    .await;

    let csr_pem = mint_csr("test-host");
    let req = RenewRequest { csr_pem };
    let client = build_mtls_client(&pki.ca_cert, &pki.agent_cert_pem, &pki.agent_key_pem);

    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/renew"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "revoked cert must not be able to renew",);

    handle.abort();
}

#[tokio::test]
async fn renew_returns_500_when_ca_not_configured() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let pki = mint_pki(&dir, "test-host");
    let port = pick_free_port().await;
    let handle = spawn_server(
        &dir,
        pki.server_cert.clone(),
        pki.server_key.clone(),
        Some(pki.ca_cert.clone()),
        None,
        None,
        None,
        port,
    )
    .await;

    let csr_pem = mint_csr("test-host");
    let req = RenewRequest { csr_pem };
    let client = build_mtls_client(&pki.ca_cert, &pki.agent_cert_pem, &pki.agent_key_pem);

    let resp = client
        .post(format!("https://localhost:{port}/v1/agent/renew"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        500,
        "no CA configured must surface 500, not silent success",
    );

    handle.abort();
}
