//! Boundary test for the agent-side `manifest_poll` worker (Option C lift
//! per the d010-feed-reducer-manifest-cache plan).
//!
//! Asserts assertion (a) of the architect's revised E2E: within one tick
//! of spawn the worker fetches `/v1/fleet.resolved` + each channel's
//! signed rollout manifest, applies the `fleet_resolved_hash` cross-check
//! discriminator, and emits `ReducerInput::ManifestSetUpdated` onto the
//! reducer's input channel.
//!
//! Assertion (b) ("subsequent dispatch successfully bootstraps") is
//! covered by the reducer-side SR-1 + SR-2 unit tests in
//! `runtime/reducer.rs` — splitting keeps each test focused on its
//! boundary and avoids spinning up the full agent runtime (activation +
//! probe workers) just to observe the bootstrap-shape.

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
use nixfleet_proto::fleet_resolved::{
    Channel, HealthGate, Host, Meta as FleetMeta, OnHealthFailure, PolicyWave, RolloutPolicy,
    Selector,
};
use nixfleet_proto::rollout_manifest::HostWave;
use nixfleet_proto::{
    FleetResolved, KeySlot, Meta as ManifestMeta, RolloutId, RolloutManifest, TrustConfig,
    TrustedPubkey,
};
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

use nixfleet_agent::runtime::workers::manifest_poll;
use nixfleet_agent::runtime::{AgentConfig, ReducerInput, ShutdownToken};
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
        .push(DnType::CommonName, "manifest-poll-ca");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "manifest-poll-server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    let mut client_params = CertificateParams::default();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "agent-manifest-poll");
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

#[derive(Clone)]
struct CpFs {
    dir: Arc<PathBuf>,
}

async fn fleet_resolved(State(state): State<CpFs>) -> Result<impl IntoResponse, StatusCode> {
    let bytes =
        std::fs::read(state.dir.join("fleet.resolved.json")).map_err(|_| StatusCode::NOT_FOUND)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(bytes)))
}

async fn fleet_resolved_sig(State(state): State<CpFs>) -> Result<impl IntoResponse, StatusCode> {
    let bytes = std::fs::read(state.dir.join("fleet.resolved.json.sig"))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(bytes)))
}

async fn rollout_manifest_route(
    State(state): State<CpFs>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let bytes = std::fs::read(state.dir.join(format!("rollouts/{rollout_id}.json")))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(bytes)))
}

async fn rollout_signature_route(
    State(state): State<CpFs>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let bytes = std::fs::read(state.dir.join(format!("rollouts/{rollout_id}.json.sig")))
        .map_err(|_| StatusCode::NOT_FOUND)?;
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

fn sign_bytes(payload: &impl serde::Serialize, signing_key: &SigningKey) -> (Vec<u8>, [u8; 64]) {
    let raw = serde_json::to_string(payload).expect("serialise");
    let canonical = canonicalize(&raw).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    (canonical.into_bytes(), sig)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_poll_emits_signed_manifest_set_within_one_tick() {
    install_crypto_provider_once();

    let cp_fs = TempDir::new().unwrap();
    let agent_state = TempDir::new().unwrap();
    let trust_dir = TempDir::new().unwrap();
    let cert_dir = TempDir::new().unwrap();

    let signing_key = fresh_signing_key();
    let signed_at = Utc::now();

    // Build a minimal FleetResolved with one channel + one host + one
    // rollout policy. compute_rollout_id_for_channel derives the rollout
    // id; the manifest projection embeds the same channel + channel_ref.
    let mut channels = std::collections::HashMap::new();
    channels.insert(
        "stable".to_string(),
        Channel {
            rollout_policy: "default".to_string(),
            reconcile_interval_minutes: 5,
            freshness_window: 60,
            signing_interval_minutes: 30,
        },
    );
    let mut rollout_policies = std::collections::HashMap::new();
    rollout_policies.insert(
        "default".to_string(),
        RolloutPolicy {
            strategy: "waves".to_string(),
            waves: vec![PolicyWave {
                selector: Selector {
                    tags: vec![],
                    tags_any: vec![],
                    hosts: vec![],
                    channel: None,
                    all: true,
                },
                soak_minutes: 5,
            }],
            health_gate: HealthGate::default(),
            on_health_failure: OnHealthFailure::Halt,
        },
    );
    let mut hosts = std::collections::HashMap::new();
    hosts.insert(
        "h1".to_string(),
        Host {
            system: "x86_64-linux".to_string(),
            tags: vec![],
            channel: "stable".to_string(),
            closure_hash: Some("closure-A".to_string()),
            pubkey: None,
            pin: None,
        },
    );
    let fleet = FleetResolved {
        schema_version: 1,
        hosts,
        channels,
        rollout_policies,
        waves: Default::default(),
        edges: vec![],
        channel_edges: vec![],
        disruption_budgets: vec![],
        meta: FleetMeta {
            schema_version: 1,
            signed_at: Some(signed_at),
            ci_commit: Some("abc1234deadbeef".to_string()),
            signature_algorithm: Some("ed25519".to_string()),
        },
    };

    let (fleet_bytes, fleet_sig) = sign_bytes(&fleet, &signing_key);
    let fleet_hash =
        nixfleet_reconciler::canonical_hash_from_bytes(&fleet_bytes).expect("compute fleet hash");

    // Project the canonical rollout id; the agent's manifest_poll uses
    // the same compute_rollout_id_for_channel call to derive what to fetch.
    let rollout_id =
        nixfleet_reconciler::compute_rollout_id_for_channel(&fleet, &fleet_hash, "stable")
            .expect("compute_rollout_id_for_channel")
            .expect("non-empty rollout for channel stable");
    let canonical_rid = RolloutId::new(&fleet.hosts["h1"].channel, "abc1234deadbeef");
    assert_eq!(
        rollout_id,
        canonical_rid.as_str(),
        "rollout_id projection matches RolloutId::new(channel, channel_ref)",
    );

    // Build the rollout manifest that pins the same fleet_resolved_hash.
    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@abc1234".to_string(),
        channel: "stable".to_string(),
        channel_ref: "abc1234deadbeef".to_string(),
        fleet_resolved_hash: fleet_hash.clone(),
        host_set: vec![HostWave {
            hostname: "h1".to_string(),
            wave_index: 0,
            target_closure: "closure-A".to_string(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: vec![],
        meta: ManifestMeta {
            schema_version: 1,
            signed_at: Some(signed_at),
            ci_commit: Some("abc1234".to_string()),
            signature_algorithm: Some("ed25519".to_string()),
        },
    };
    let (manifest_bytes, manifest_sig) = sign_bytes(&manifest, &signing_key);

    // Write signed bytes to CP-side tempdir under the same layout CP
    // serves: `<dir>/fleet.resolved.json` + `.sig` and
    // `<dir>/rollouts/<rollout_id>.json` + `.sig`.
    std::fs::write(cp_fs.path().join("fleet.resolved.json"), &fleet_bytes).unwrap();
    std::fs::write(cp_fs.path().join("fleet.resolved.json.sig"), fleet_sig).unwrap();
    std::fs::create_dir_all(cp_fs.path().join("rollouts")).unwrap();
    std::fs::write(
        cp_fs.path().join(format!("rollouts/{rollout_id}.json")),
        &manifest_bytes,
    )
    .unwrap();
    std::fs::write(
        cp_fs.path().join(format!("rollouts/{rollout_id}.json.sig")),
        manifest_sig,
    )
    .unwrap();

    // Write agent trust.json (pointing at the release-signing key).
    let trust = build_trust_config(&signing_key);
    let trust_path = trust_dir.path().join("trust.json");
    std::fs::write(&trust_path, serde_json::to_string(&trust).unwrap()).unwrap();

    // Spin up the mTLS CP-side test server with the four routes the
    // manifest_poll worker hits.
    let certs = mint_certs();
    let server_config = build_mtls_server_config(&certs);
    let rustls_config = RustlsConfig::from_config(Arc::new(server_config));

    let cp_state = CpFs {
        dir: Arc::new(cp_fs.path().to_path_buf()),
    };
    let app = Router::new()
        .route("/v1/fleet.resolved", get(fleet_resolved))
        .route("/v1/fleet.resolved/sig", get(fleet_resolved_sig))
        .route("/v1/rollouts/{rollout_id}", get(rollout_manifest_route))
        .route(
            "/v1/rollouts/{rollout_id}/sig",
            get(rollout_signature_route),
        )
        .with_state(cp_state);

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

    // Spawn the manifest_poll worker via the test-only spawn variant so
    // the trust path is tempdir-rooted (production uses
    // AgentConfig::trust_file threaded from --trust-file).
    let cfg = AgentConfig {
        control_plane_url: format!("https://localhost:{port}"),
        machine_id: "h1".to_string(),
        state_dir: agent_state.path().to_path_buf(),
        trust_file: agent_state.path().join("trust.json"),
        manifest_freshness_window_secs: 3600,
        ca_cert: Some(ca_path),
        client_cert: Some(cert_path),
        client_key: Some(key_path),
    };
    let clock: ClockHandle = Arc::new(SystemClock::new());
    let (input_tx, mut input_rx) = mpsc::channel::<ReducerInput>(8);
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown = ShutdownToken::__test_only_from_rx(shutdown_rx);
    let worker = manifest_poll::spawn_with_trust_path(cfg, trust_path, clock, input_tx, shutdown);

    // First tick fires immediately on spawn (no startup delay); the
    // worker should emit ManifestSetUpdated within seconds.
    let input = tokio::time::timeout(Duration::from_secs(10), input_rx.recv())
        .await
        .expect("ManifestSetUpdated received within 10s")
        .expect("input channel not closed");

    match input {
        ReducerInput::ManifestSetUpdated(set) => {
            assert_eq!(
                set.fleet.inner().channels.keys().collect::<Vec<_>>(),
                vec!["stable"],
                "fleet carries the expected channel",
            );
            let rollout = set.rollouts.get("stable").expect("rollout for stable");
            assert_eq!(rollout.inner().channel, "stable");
            assert_eq!(rollout.inner().channel_ref, "abc1234deadbeef");
            assert_eq!(
                rollout.inner().fleet_resolved_hash,
                fleet_hash,
                "rollout's fleet_resolved_hash matches the served fleet's hash (discriminator pass)",
            );
            assert_eq!(rollout.inner().host_set.len(), 1);
            assert_eq!(rollout.inner().host_set[0].target_closure, "closure-A");
        }
        _ => panic!("expected ManifestSetUpdated, got a different ReducerInput variant"),
    }

    // Disk-cache write-through landed at the expected paths.
    assert!(
        agent_state
            .path()
            .join("fleet/fleet.resolved.json")
            .exists(),
        "fleet cache written to fleet/ subdir",
    );
    assert!(
        agent_state
            .path()
            .join(format!("rollouts/{rollout_id}.json"))
            .exists(),
        "rollout cache written to rollouts/ subdir",
    );

    drop(shutdown_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), worker).await;
    server_handle.abort();
}
