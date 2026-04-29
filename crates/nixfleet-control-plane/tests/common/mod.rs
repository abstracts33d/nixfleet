//! Shared TLS / cert / port helpers for CP integration tests.
//!
//! Cargo recognises `tests/common/mod.rs` (subdirectory form) as
//! a module rather than a separate test binary. Each integration
//! test file imports it with `mod common;` at the top.
//!
//! `#[allow(dead_code)]` on every item: not every test uses every
//! helper, and cargo emits per-binary unused-code warnings.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Once;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use reqwest::{Certificate as ReqwestCert, Identity};
use tempfile::TempDir;
use tokio::net::TcpListener;

/// Install the rustls aws-lc-rs crypto provider exactly once per
/// test process, and wire `tracing_subscriber` so `RUST_LOG=info`
/// surfaces dispatch / reconcile traces during triage.
pub fn install_crypto_provider_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .try_init();
    });
}

/// Bind 127.0.0.1:0 and return the OS-allocated port. Caller is
/// responsible for racing the rebind in the spawn step.
pub async fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Write `contents` to `dir/name` and return the absolute path.
pub fn write_pem(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// Same as [`write_pem`] but for raw bytes (signature files,
/// canonicalised JSON sig blobs, etc.).
pub fn write_bytes(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// Mint a self-signed CA, a server cert (CN=test-cp-server,
/// SAN=localhost) and a client cert (CN=`client_cn`) under that
/// CA. Writes all five PEM files into `dir`.
///
/// Returns paths in the order: `(ca, server_cert, server_key,
/// client_cert, client_key)`.
pub fn mint_ca_and_certs(
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

/// Build a `reqwest::Client` configured for mTLS against the
/// CP: trusts `ca`, presents the `(client_cert, client_key)`
/// identity. Use [`mint_ca_and_certs`] to produce the inputs.
pub fn build_mtls_client(
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
