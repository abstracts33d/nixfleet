//! DEFECT-003 regression test.
//!
//! Spins up an `axum-server` configured to require mTLS via rustls'
//! `WebPkiClientVerifier`, spawns the agent's longpoll worker pointed
//! at it, and asserts the worker's `/v1/agent/dispatch` request
//! arrives at the handler within 5 seconds.
//!
//! The arriving request IS the assertion. rustls routes a request to
//! the handler only after the client-cert verification passes. A worker
//! building a bare `reqwest::Client` (the original DEFECT-003 regression)
//! would be cut off at the TLS handshake before reaching the handler —
//! the `tokio::time::timeout` below would expire and the test would fail
//! with a clear regression message.
//!
//! Same delegation pattern is exercised by all three workers
//! (longpoll / heartbeat / outbound); one regression test on longpoll
//! covers the shape.
//!
//! NB: `Arc<oneshot::Sender>` is not directly usable because `send`
//! consumes the sender. We wrap it in `Mutex<Option<Sender>>` and
//! `take()` on first signal; the handler is fire-once.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use axum::{Router, http::StatusCode, routing::get};
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use rustls_pki_types::pem::PemObject;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use nixfleet_agent::runtime::workers::longpoll;
use nixfleet_agent::runtime::{AgentConfig, ReducerInput};
use nixfleet_proto::clock::{ClockHandle, SystemClock};

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

struct MintedCerts {
    ca_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

fn mint_certs() -> MintedCerts {
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "defect-003-ca");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "defect-003-server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    let mut client_params = CertificateParams::default();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "agent-enrollment-cert");
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_key = KeyPair::generate().unwrap();
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    MintedCerts {
        ca_pem: ca_cert.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

fn build_mtls_server_config(certs: &MintedCerts) -> ServerConfig {
    let mut roots = RootCertStore::empty();
    for cert in rustls_pki_types::CertificateDer::pem_slice_iter(certs.ca_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .unwrap();

    let cert_chain: Vec<_> =
        rustls_pki_types::CertificateDer::pem_slice_iter(certs.server_cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap();
    let key =
        rustls_pki_types::PrivateKeyDer::from_pem_slice(certs.server_key_pem.as_bytes()).unwrap();

    ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .unwrap()
}

async fn wait_for_listener(port: u16) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("listener on 127.0.0.1:{port} did not bind within 5s");
}

fn make_shutdown_token() -> (nixfleet_agent::runtime::ShutdownToken, oneshot::Sender<()>) {
    // ShutdownToken wraps a `oneshot::Receiver<()>`; the only public
    // constructor is `runtime::spawn`'s internal use. We reach in via
    // the same one-shot channel: drop-on-send fires the worker's
    // shutdown arm. There's no public `new`, so we shadow the field
    // shape by constructing via the type's serialization helper —
    // simpler: send via a regular channel and let the worker exit by
    // observing all senders dropped (Err on the receiver).
    let (tx, rx) = oneshot::channel::<()>();
    // SAFETY: ShutdownToken is `#[repr(transparent)]` over the
    // receiver in practice (single-field tuple struct). We use a
    // memory-safe in-crate constructor instead — see
    // `nixfleet_agent::runtime::ShutdownToken::__test_new` below.
    //
    // The above is a placeholder until a `#[cfg(test)] pub fn new`
    // exists. For now we construct via the public `into_inner`-mirror
    // helper added by this test commit.
    let token = nixfleet_agent::runtime::ShutdownToken::__test_only_from_rx(rx);
    (token, tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn longpoll_worker_presents_mtls_cert_on_dispatch_request() {
    install_crypto_provider_once();
    let dir = tempfile::tempdir().unwrap();
    let certs = mint_certs();

    let server_config = build_mtls_server_config(&certs);
    let rustls_config = RustlsConfig::from_config(Arc::new(server_config));

    let (req_tx, req_rx) = oneshot::channel::<()>();
    let req_tx: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(Some(req_tx)));

    let app = Router::new().route(
        "/v1/agent/dispatch",
        get({
            let req_tx = Arc::clone(&req_tx);
            move || {
                let req_tx = Arc::clone(&req_tx);
                async move {
                    if let Some(t) = req_tx.lock().unwrap().take() {
                        let _ = t.send(());
                    }
                    StatusCode::NO_CONTENT
                }
            }
        }),
    );

    let port = pick_free_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let server_handle = tokio::spawn(async move {
        let _ = axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await;
    });

    wait_for_listener(port).await;

    // Write the client cert pair where AgentConfig points. These are
    // the same paths the operator-side CLI hands to the agent via
    // `--ca-cert` / `--client-cert` / `--client-key`.
    let ca_path = write_pem(&dir, "ca.pem", &certs.ca_pem);
    let cert_path = write_pem(&dir, "agent.pem", &certs.client_cert_pem);
    let key_path = write_pem(&dir, "agent.key", &certs.client_key_pem);

    let cfg = AgentConfig {
        control_plane_url: format!("https://localhost:{port}"),
        machine_id: "defect-003-agent".to_string(),
        state_dir: dir.path().to_path_buf(),
        ca_cert: Some(ca_path),
        client_cert: Some(cert_path),
        client_key: Some(key_path),
    };

    let (input_tx, _input_rx) = mpsc::channel::<ReducerInput>(8);
    let (shutdown, _shutdown_tx) = make_shutdown_token();
    let clock: ClockHandle = Arc::new(SystemClock::new());

    let worker_handle = longpoll::spawn(cfg, clock, input_tx, shutdown);

    // The longpoll worker fires its first GET /v1/agent/dispatch?wait=60
    // immediately. mTLS handshake must succeed for the request to reach
    // the handler — which then signals via req_tx.
    tokio::time::timeout(Duration::from_secs(5), req_rx)
        .await
        .expect(
            "longpoll worker did not reach mTLS-required server within 5s — \
             DEFECT-003 regressed: worker is building a bare reqwest::Client \
             instead of using comms::build_client with mTLS",
        )
        .unwrap();

    // Tear down: dropping `_shutdown_tx` closes the oneshot, worker's
    // select arm fires.
    drop(_shutdown_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), worker_handle).await;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_recovery_handshake_presents_mtls_cert() {
    // D-005 regression test. Same shape as DEFECT-003's longpoll
    // variant: arriving `POST /v1/agent/heartbeat` IS the assertion
    // because rustls only delivers it to the handler after client-cert
    // verification passes. Pre-D-005 the recovery handshake built a
    // bare `reqwest::Client::builder()` and would fail at the TLS
    // handshake.
    install_crypto_provider_once();
    let dir = tempfile::tempdir().unwrap();
    let certs = mint_certs();

    let server_config = build_mtls_server_config(&certs);
    let rustls_config = RustlsConfig::from_config(Arc::new(server_config));

    let (req_tx, req_rx) = oneshot::channel::<()>();
    let req_tx: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(Some(req_tx)));

    let app = Router::new().route(
        "/v1/agent/heartbeat",
        axum::routing::post({
            let req_tx = Arc::clone(&req_tx);
            move || {
                let req_tx = Arc::clone(&req_tx);
                async move {
                    if let Some(t) = req_tx.lock().unwrap().take() {
                        let _ = t.send(());
                    }
                    // Empty 200 with no Replay-From header — recovery
                    // path treats that as "no drift".
                    (StatusCode::OK, axum::Json(serde_json::json!({})))
                }
            }
        }),
    );

    let port = pick_free_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let server_handle = tokio::spawn(async move {
        let _ = axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await;
    });

    wait_for_listener(port).await;

    let ca_path = write_pem(&dir, "ca.pem", &certs.ca_pem);
    let cert_path = write_pem(&dir, "agent.pem", &certs.client_cert_pem);
    let key_path = write_pem(&dir, "agent.key", &certs.client_key_pem);
    let current_system = dir.path().join("missing-current-system");

    let clock: ClockHandle = Arc::new(SystemClock::new());

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        nixfleet_agent::runtime::boot_recovery_handshake(
            &format!("https://localhost:{port}"),
            "defect-005-agent",
            &clock,
            &current_system,
            Some(&ca_path),
            Some(&cert_path),
            Some(&key_path),
        ),
    )
    .await
    .expect(
        "boot_recovery_handshake did not reach mTLS-required server within 5s — \
         D-005 regressed: recovery::handshake is building a bare reqwest::Client \
         instead of using comms::build_client with mTLS",
    );

    // Outcome.replay_from is None (handler emits no header) but the
    // arriving request — captured via the req_tx oneshot — proves
    // mTLS succeeded.
    let _ = outcome;
    tokio::time::timeout(Duration::from_secs(1), req_rx)
        .await
        .expect("handler did not signal request arrival")
        .unwrap();

    server_handle.abort();
}
