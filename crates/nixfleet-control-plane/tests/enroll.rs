//! Integration tests for `/v1/enroll`.

mod common;

use std::path::PathBuf;

use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use common::{install_crypto_provider_once, pick_free_port, wait_for_listener_ready};
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_control_plane::server;
use nixfleet_proto::enroll_wire::{
    BootstrapToken, EnrollRequest, EnrollResponse, TokenClaims,
};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CertificateSigningRequest, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PublicKeyData,
};
use tempfile::TempDir;

fn write(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

fn mint_fleet_ca(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
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

    let mut server_params =
        CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "test-cp-server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    let ca_cert_path = dir.path().join("ca.pem");
    let ca_key_path = dir.path().join("ca.key");
    let server_cert_path = dir.path().join("server.pem");
    let server_key_path = dir.path().join("server.key");

    write(&ca_cert_path, &ca_cert.pem());
    write(&ca_key_path, &ca_key.serialize_pem());
    write(&server_cert_path, &server_cert.pem());
    write(&server_key_path, &server_key.serialize_pem());

    (ca_cert_path, ca_key_path, server_cert_path, server_key_path)
}

/// LOADBEARING: place trust.json next to ca.pem; handler looks up dirname(fleet-ca-cert)/trust.json.
fn write_trust_json(dir: &TempDir, org_root_pubkey_b64: &str) -> PathBuf {
    let path = dir.path().join("trust.json");
    let contents = format!(
        r#"{{
  "schemaVersion": 1,
  "ciReleaseKey": {{ "current": null, "previous": null, "rejectBefore": null }},
  "cacheKeys": [],
  "orgRootKey": {{
    "current": {{ "algorithm": "ed25519", "public": "{org_root_pubkey_b64}" }},
    "previous": null,
    "rejectBefore": null
  }}
}}"#
    );
    write(&path, &contents);
    path
}

// FOOTGUN: fingerprint must be over parsed-CSR `der_bytes`, not KeyPair `public_key_der()` — different framings.
fn mint_csr(hostname: &str) -> (String, Vec<u8>, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, hostname);
    let csr: CertificateSigningRequest = params.serialize_request(&key).unwrap();
    let pem = csr.pem().unwrap();

    let parsed = rcgen::CertificateSigningRequestParams::from_pem(&pem).unwrap();
    let pubkey_der: Vec<u8> = parsed.public_key.der_bytes().to_vec();
    let digest = sha2::Sha256::digest(&pubkey_der);
    let fingerprint = base64::engine::general_purpose::STANDARD.encode(digest);

    (pem, pubkey_der, fingerprint)
}
use sha2::Digest;

fn sign_token(claims: &TokenClaims, signing_key: &SigningKey, version: u32) -> BootstrapToken {
    let claims_json = serde_json::to_string(claims).unwrap();
    let canonical = nixfleet_canonicalize::canonicalize(&claims_json).unwrap();
    let signature = signing_key.sign(canonical.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    BootstrapToken {
        version,
        claims: claims.clone(),
        signature: sig_b64,
    }
}

fn random_nonce() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

async fn spawn_server(
    listen: std::net::SocketAddr,
    server_cert: PathBuf,
    server_key: PathBuf,
    fleet_ca_cert: PathBuf,
    fleet_ca_key: PathBuf,
    audit_log: PathBuf,
    obs_dir: &TempDir,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let artifact = obs_dir.path().join("fleet.resolved.json");
    write(&artifact, "{}");
    let signature = obs_dir.path().join("fleet.resolved.json.sig");
    write(&signature, "");
    // GOTCHA: this trust_path is the legacy --trust-file flag, distinct from the enroll handler lookup.
    let trust = obs_dir.path().join("trust-stub.json");
    write(
        &trust,
        r#"{"ciReleaseKey":{"current":null,"previous":null,"rejectBefore":null}}"#,
    );
    let observed = obs_dir.path().join("observed.json");
    write(
        &observed,
        r#"{"channelRefs":{},"lastRolledRefs":{},"hostState":{},"activeRollouts":[]}"#,
    );
    let db_path = obs_dir.path().join("state.db");

    let args = server::ServeArgs {
        listen,
        tls_cert: server_cert,
        tls_key: server_key,
        client_ca: None,
        fleet_ca_cert: Some(fleet_ca_cert),
        fleet_ca_key: Some(fleet_ca_key),
        audit_log_path: Some(audit_log),
        artifact_path: artifact,
        signature_path: signature,
        trust_path: trust,
        observed_path: observed,
        confirm_deadline_secs: 120,
        db_path: Some(db_path),
        ..Default::default()
    };
    let port = listen.port();
    let handle = tokio::spawn(server::serve(args));
    wait_for_listener_ready(port, &handle).await;
    handle
}

#[tokio::test]
async fn enroll_happy_path_signs_cert() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca_cert, ca_key, server_cert, server_key) = mint_fleet_ca(&dir);
    let audit_log = dir.path().join("issuance.log");

    let mut rng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut rng);
    let pubkey_b64 = base64::engine::general_purpose::STANDARD
        .encode(signing_key.verifying_key().to_bytes());
    let _trust_path = write_trust_json(&dir, &pubkey_b64);

    let port = pick_free_port().await;
    let listen = format!("127.0.0.1:{port}").parse().unwrap();
    let handle = spawn_server(
        listen, server_cert, server_key, ca_cert.clone(), ca_key, audit_log, &dir,
    )
    .await;

    let (csr_pem, _pubkey_der, fingerprint) = mint_csr("test-host");
    let now = Utc::now();
    let claims = TokenClaims {
        hostname: "test-host".to_string(),
        expected_pubkey_fingerprint: fingerprint,
        issued_at: now - ChronoDuration::seconds(5),
        expires_at: now + ChronoDuration::hours(1),
        nonce: random_nonce(),
    };
    let token = sign_token(&claims, &signing_key, 1);

    let ca_pem = std::fs::read(&ca_cert).unwrap();
    let ca_certb = reqwest::Certificate::from_pem(&ca_pem).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_certb)
        .build()
        .unwrap();

    let req = EnrollRequest { token, csr_pem };
    let resp = client
        .post(format!("https://localhost:{port}/v1/enroll"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "enroll happy path returned non-200");

    let body: EnrollResponse = resp.json().await.unwrap();
    assert!(body.cert_pem.contains("BEGIN CERTIFICATE"));
    assert!(body.not_after > now);

    handle.abort();
}

#[tokio::test]
async fn enroll_rejects_tampered_signature() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca_cert, ca_key, server_cert, server_key) = mint_fleet_ca(&dir);
    let audit_log = dir.path().join("issuance.log");

    let mut rng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut rng);
    let pubkey_b64 = base64::engine::general_purpose::STANDARD
        .encode(signing_key.verifying_key().to_bytes());
    let _trust_path = write_trust_json(&dir, &pubkey_b64);

    let port = pick_free_port().await;
    let handle = spawn_server(
        format!("127.0.0.1:{port}").parse().unwrap(),
        server_cert,
        server_key,
        ca_cert.clone(),
        ca_key,
        audit_log,
        &dir,
    )
    .await;

    let (csr_pem, _pubkey_der, fingerprint) = mint_csr("test-host");
    let now = Utc::now();
    let claims = TokenClaims {
        hostname: "test-host".to_string(),
        expected_pubkey_fingerprint: fingerprint,
        issued_at: now - ChronoDuration::seconds(5),
        expires_at: now + ChronoDuration::hours(1),
        nonce: random_nonce(),
    };
    let mut token = sign_token(&claims, &signing_key, 1);
    let mut sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&token.signature)
        .unwrap();
    let last = sig_bytes.len() - 1;
    sig_bytes[last] ^= 0x01;
    token.signature = base64::engine::general_purpose::STANDARD.encode(&sig_bytes);

    let ca_pem = std::fs::read(&ca_cert).unwrap();
    let ca_certb = reqwest::Certificate::from_pem(&ca_pem).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_certb)
        .build()
        .unwrap();

    let req = EnrollRequest { token, csr_pem };
    let resp = client
        .post(format!("https://localhost:{port}/v1/enroll"))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "tampered signature should 401");

    handle.abort();
}

#[tokio::test]
async fn enroll_rejects_replayed_nonce() {
    install_crypto_provider_once();

    let dir = TempDir::new().unwrap();
    let (ca_cert, ca_key, server_cert, server_key) = mint_fleet_ca(&dir);
    let audit_log = dir.path().join("issuance.log");

    let mut rng = rand::thread_rng();
    let signing_key = SigningKey::generate(&mut rng);
    let pubkey_b64 = base64::engine::general_purpose::STANDARD
        .encode(signing_key.verifying_key().to_bytes());
    let _trust_path = write_trust_json(&dir, &pubkey_b64);

    let port = pick_free_port().await;
    let handle = spawn_server(
        format!("127.0.0.1:{port}").parse().unwrap(),
        server_cert,
        server_key,
        ca_cert.clone(),
        ca_key,
        audit_log,
        &dir,
    )
    .await;

    let (csr_pem, _pubkey_der, fingerprint) = mint_csr("test-host");
    let now = Utc::now();
    let claims = TokenClaims {
        hostname: "test-host".to_string(),
        expected_pubkey_fingerprint: fingerprint,
        issued_at: now - ChronoDuration::seconds(5),
        expires_at: now + ChronoDuration::hours(1),
        nonce: random_nonce(),
    };
    let token = sign_token(&claims, &signing_key, 1);

    let ca_pem = std::fs::read(&ca_cert).unwrap();
    let ca_certb = reqwest::Certificate::from_pem(&ca_pem).unwrap();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .add_root_certificate(ca_certb)
        .build()
        .unwrap();

    let req1 = EnrollRequest {
        token: token.clone(),
        csr_pem: csr_pem.clone(),
    };
    let resp1 = client
        .post(format!("https://localhost:{port}/v1/enroll"))
        .json(&req1)
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);

    let req2 = EnrollRequest { token, csr_pem };
    let resp2 = client
        .post(format!("https://localhost:{port}/v1/enroll"))
        .json(&req2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 409, "replayed nonce should 409");

    handle.abort();
}
