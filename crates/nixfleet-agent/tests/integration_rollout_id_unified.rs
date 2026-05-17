//! End-to-end round-trip for the canonical RolloutId format (RFC-0012 §6.3).
//!
//! Producer writes the signed manifest at `{channel}@{channel_ref}.json`;
//! CP serves it through `/v1/rollouts/{rolloutId}`; agent's
//! `ManifestCache::ensure_for_dispatch` fetches via mTLS, verifies signature,
//! discriminates the parsed identity against the advertised id, asserts
//! target_closure match, writes through to disk. Failure at any seam between
//! producer / route / consumer surfaces here.
//!
//! The CP-side route validator's character-class behaviour is covered
//! exhaustively by `nixfleet_control_plane::server::routes::rollouts::tests`;
//! this test exercises the agent's full path including the actual HTTP
//! fetch, against a minimal axum-server whose handler mirrors the production
//! route's filesystem layout.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::Utc;
use ed25519_dalek::ed25519::signature::rand_core::{OsRng, RngCore};
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_canonicalize::canonicalize;
use nixfleet_proto::fleet_resolved::{HealthGate, Meta};
use nixfleet_proto::rollout_manifest::HostWave;
use nixfleet_proto::{KeySlot, RolloutId, RolloutManifest, TrustConfig, TrustedPubkey};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use rustls_pki_types::pem::PemObject;
use tempfile::TempDir;
use tokio::net::TcpListener;

use nixfleet_agent::comms;
use nixfleet_agent::manifest_cache::ManifestCache;

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
        .push(DnType::CommonName, "d010-ca");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "d010-server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    let mut client_params = CertificateParams::default();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "agent-d010");
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

fn write_pem(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// Mirrors `nixfleet_control_plane::server::routes::rollouts::looks_like_rollout_id`.
/// The CP-side validator's coverage lives at its home (10 unit tests in the
/// route file); the duplicate proves the agent's HTTP request reaches a
/// CP-shaped handler and not just an unguarded fileserver.
fn looks_like_rollout_id(s: &str) -> bool {
    let Some((channel, channel_ref)) = s.split_once('@') else {
        return false;
    };
    if channel.is_empty() || channel_ref.is_empty() || channel_ref.contains('@') {
        return false;
    }
    if !channel
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return false;
    }
    channel_ref
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[derive(Clone)]
struct RolloutFsState {
    dir: Arc<PathBuf>,
}

async fn rollout_manifest_route(
    State(state): State<RolloutFsState>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    if !looks_like_rollout_id(&rollout_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let path = state.dir.join(format!("{rollout_id}.json"));
    let bytes = std::fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(bytes)))
}

async fn rollout_signature_route(
    State(state): State<RolloutFsState>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    if !looks_like_rollout_id(&rollout_id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let path = state.dir.join(format!("{rollout_id}.json.sig"));
    let bytes = std::fs::read(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    })?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(bytes)))
}

fn fresh_signing_key() -> SigningKey {
    let mut seed = [0u8; 32];
    OsRng.try_fill_bytes(&mut seed).expect("OS CSPRNG");
    SigningKey::from_bytes(&seed)
}

fn build_trust_config(signing_key: &SigningKey) -> TrustConfig {
    TrustConfig {
        schema_version: TrustConfig::CURRENT_SCHEMA_VERSION,
        ci_release_key: KeySlot {
            current: Some(TrustedPubkey {
                algorithm: "ed25519".to_string(),
                public: BASE64_STANDARD.encode(signing_key.verifying_key().as_bytes()),
            }),
            previous: None,
            reject_before: None,
            successor: None,
            retire_at: None,
        },
        cache_keys: Vec::new(),
        org_root_key: None,
        root_ca_pem: None,
        issuance_ca_pems: Vec::new(),
    }
}

fn sign_manifest(manifest: &RolloutManifest, signing_key: &SigningKey) -> (Vec<u8>, [u8; 64]) {
    let raw = serde_json::to_string(manifest).expect("serialise manifest");
    let canonical = canonicalize(&raw).expect("canonicalize manifest");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    (canonical.into_bytes(), sig)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_for_dispatch_round_trips_canonical_rollout_id_via_http() {
    install_crypto_provider_once();

    let cp_rollouts = TempDir::new().unwrap();
    let agent_state = TempDir::new().unwrap();
    let trust_dir = TempDir::new().unwrap();
    let cert_dir = TempDir::new().unwrap();

    let signing_key = fresh_signing_key();
    let signed_at = Utc::now();

    // Producer-side projection: nixfleet-release sets channel + channel_ref
    // from the resolved fleet's ci_commit; the rollout_id is the canonical
    // RFC-0012 §6.3 composite.
    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@abc1234".into(),
        channel: "stable".into(),
        channel_ref: "abc1234deadbeef".into(),
        fleet_resolved_hash: "1111111111111111111111111111111111111111111111111111111111111111"
            .into(),
        host_set: vec![HostWave {
            hostname: "h1".into(),
            wave_index: 0,
            target_closure: "closure-A".into(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(signed_at),
            ci_commit: Some("abc1234".into()),
            signature_algorithm: Some("ed25519".into()),
        },
    };

    let rollout_id = RolloutId::new(&manifest.channel, &manifest.channel_ref);
    assert_eq!(
        rollout_id.as_str(),
        "stable@abc1234deadbeef",
        "RolloutId composite matches the canonical format",
    );

    let (manifest_bytes, sig_bytes) = sign_manifest(&manifest, &signing_key);

    // Producer-side filename convention. nixfleet-release writes manifests
    // to `{release_dir}/rollouts/<rollout_id>.json`; CP's rollouts_dir
    // serves the same layout. Both sides derive the filename via
    // `RolloutId::new(channel, channel_ref).as_str()`, so they agree by
    // construction.
    let producer_filename = format!("{}.json", rollout_id.as_str());
    let sig_filename = format!("{}.json.sig", rollout_id.as_str());
    std::fs::write(cp_rollouts.path().join(&producer_filename), &manifest_bytes).unwrap();
    std::fs::write(cp_rollouts.path().join(&sig_filename), sig_bytes).unwrap();

    let trust = build_trust_config(&signing_key);
    let trust_path = trust_dir.path().join("trust.json");
    std::fs::write(&trust_path, serde_json::to_string(&trust).unwrap()).unwrap();

    // CP-side: mTLS server with the canonical /v1/rollouts/{id} +
    // /v1/rollouts/{id}/sig route shape.
    let certs = mint_certs();
    let server_config = build_mtls_server_config(&certs);
    let rustls_config = RustlsConfig::from_config(Arc::new(server_config));

    let fs_state = RolloutFsState {
        dir: Arc::new(cp_rollouts.path().to_path_buf()),
    };

    let app = Router::new()
        .route("/v1/rollouts/{rollout_id}", get(rollout_manifest_route))
        .route(
            "/v1/rollouts/{rollout_id}/sig",
            get(rollout_signature_route),
        )
        .with_state(fs_state);

    let port = pick_free_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let server_handle = tokio::spawn(async move {
        let _ = axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await;
    });

    wait_for_listener(port).await;

    let ca_path = write_pem(&cert_dir, "ca.pem", &certs.ca_pem);
    let cert_path = write_pem(&cert_dir, "agent.pem", &certs.client_cert_pem);
    let key_path = write_pem(&cert_dir, "agent.key", &certs.client_key_pem);

    let client = comms::build_client(Some(&ca_path), Some(&cert_path), Some(&key_path))
        .expect("build mTLS client");

    // Agent-side: ManifestCache pointed at agent state_dir for the disk
    // cache + the trust.json file holding the matching signing key.
    let cache = ManifestCache::new(agent_state.path(), &trust_path);
    let cp_url = format!("https://localhost:{port}");

    let verified = tokio::time::timeout(
        Duration::from_secs(10),
        cache.ensure_for_dispatch(&client, &cp_url, rollout_id.as_str(), "h1", "closure-A"),
    )
    .await
    .expect("round-trip completes within 10s")
    .expect("ensure_for_dispatch returns Ok");

    let result = verified.inner();
    assert_eq!(result.channel, "stable");
    assert_eq!(result.channel_ref, "abc1234deadbeef");
    assert_eq!(result.host_set.len(), 1);
    assert_eq!(result.host_set[0].target_closure, "closure-A");

    // Disk-cache write-through landed at the canonical filename.
    let cached_path = agent_state
        .path()
        .join("rollouts")
        .join(format!("{}.json", rollout_id.as_str()));
    assert!(
        cached_path.exists(),
        "disk cache wrote to {} (canonical filename round-trip)",
        cached_path.display(),
    );

    server_handle.abort();
}
