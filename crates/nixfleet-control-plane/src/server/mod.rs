//! Long-running TLS server: router + listener + reconcile loop + polls.

mod middleware;
mod route_error;
mod routes;
mod state;

pub use state::{AppState, ClosureUpstream, IssuancePaths, ServeArgs, VerifiedFleetSnapshot};

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use axum::middleware::Next;
use axum::routing::{get, post};
use tokio_util::sync::CancellationToken;

use crate::auth::auth_cn::MtlsAcceptor;

/// 30s = systemd `TimeoutStopSec=` default; stay under to avoid SIGKILL.
const TASK_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(30);

/// Listener drain budget; remaining 5s of `TASK_SHUTDOWN_DEADLINE` is for background drain.
const HTTP_DRAIN_DEADLINE: Duration = Duration::from_secs(25);

/// `/healthz` outside `/v1`; `/v1/enroll` is anonymous; all other `/v1/*` require mTLS.
/// `/v1/*` is gated by `require_ready_layer` (#95) so agents get 503 + Retry-After
/// until the first signed artifact is verified - no stale-state serving.
fn build_router(state: Arc<AppState>) -> Router {
    let strict = state.strict;
    let auth_state = state.clone();
    let ready_state = state.clone();

    let anonymous_v1 = Router::new().route("/v1/enroll", post(routes::enrollment::enroll));

    let authenticated_v1 = Router::new()
        .route("/v1/whoami", get(routes::status::whoami))
        // RFC-0005 §4 wire protocol (replaces the v0.1 checkin / confirm
        // pull pipeline — agents now POST events as they happen and
        // long-poll for dispatches).
        .route("/v1/agent/events", post(routes::events::events))
        .route("/v1/agent/heartbeat", post(routes::heartbeat::heartbeat))
        .route("/v1/agent/dispatch", get(routes::dispatch::dispatch))
        .route(
            "/v1/agent/closure/{hash}",
            get(routes::status::closure_proxy),
        )
        .route("/v1/agent/renew", post(routes::enrollment::renew))
        .route("/v1/channels/{name}", get(routes::status::channel_status))
        .route("/v1/hosts", get(routes::status::hosts_status))
        .route("/metrics", get(routes::metrics::metrics_handler))
        .route("/v1/rollouts", get(routes::rollouts::list_active))
        .route("/v1/deferrals", get(routes::deferrals::list))
        .route("/v1/fleet.resolved", get(routes::fleet::artifact))
        .route("/v1/fleet.resolved/sig", get(routes::fleet::signature))
        .route("/v1/rollouts/{rolloutId}", get(routes::rollouts::manifest))
        .route(
            "/v1/rollouts/{rolloutId}/sig",
            get(routes::rollouts::signature),
        )
        .route(
            "/v1/rollouts/{rolloutId}/lifecycle",
            get(routes::rollouts::lifecycle),
        )
        .route(
            "/v1/rollouts/{rolloutId}/hosts",
            get(routes::rollouts::hosts),
        )
        .route(
            "/v1/rollouts/{rolloutId}/events",
            get(routes::rollouts::events),
        )
        .layer(axum::middleware::from_fn(move |req, next| {
            let s = auth_state.clone();
            async move { middleware::require_cn_layer(s, req, next).await }
        }));

    let v1_routes = anonymous_v1
        .merge(authenticated_v1)
        .layer(axum::middleware::from_fn(move |req, next| {
            version_layer(strict, req, next)
        }))
        .layer(axum::middleware::from_fn(move |req, next| {
            let s = ready_state.clone();
            async move { middleware::require_ready_layer(s, req, next).await }
        }));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .merge(v1_routes)
        .with_state(state)
}

async fn version_layer(
    strict: bool,
    req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    middleware::protocol_version_middleware(strict, req, next).await
}

pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    if args.strict {
        let mut missing: Vec<&str> = Vec::new();
        if args.client_ca.is_none() {
            missing.push("--client-ca (mTLS verification disabled - TLS-only mode)");
        }
        if args.revocations.is_none() {
            missing.push("--revocations-{artifact,signature}-url (revocations polling disabled - previously-revoked certs become valid again after CP rebuild)");
        }
        if args.bootstrap_nonces.is_none() {
            missing.push("--bootstrap-nonces-{artifact,signature}-url (bootstrap-nonces polling disabled - replay-after-DB-wipe protection absent, nixfleet#96)");
        }
        // RFC-0010 §1.5.1: file-backed CA leaves signing material on disk;
        // production deployments MUST use the TPM backend or opt-in explicitly.
        let tpm_configured = args.tpm_ca_pubkey_raw.is_some() && args.tpm_ca_sign_wrapper.is_some();
        let file_only = args.fleet_ca_key.is_some() && !tpm_configured;
        if file_only && !args.allow_file_ca_key {
            missing.push("--tpm-ca-pubkey-raw + --tpm-ca-sign-wrapper (file-backed --fleet-ca-key keeps signing material on disk; pass --allow-file-ca-key to opt out, RFC-0010 §1.5.1)");
        }
        if !missing.is_empty() {
            anyhow::bail!(
                "--strict refuses to start: the following security flags are unset:\n  - {}\n\
                 Either set the missing flags or drop --strict for development.",
                missing.join("\n  - "),
            );
        }
    }

    let db = if let Some(path) = &args.db_path {
        let db = crate::db::Db::open(path)?;
        db.migrate()?;
        tracing::info!(path = %path.display(), "sqlite opened + migrated");
        Some(Arc::new(db))
    } else {
        None
    };

    let closure_upstream = if let Some(base_url) = &args.closure_upstream {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("build closure proxy client: {e}"))?;
        Some(ClosureUpstream {
            base_url: base_url.clone(),
            client,
        })
    } else {
        None
    };
    let revocations_required = args.revocations.is_some();
    let bootstrap_nonces_required = args.bootstrap_nonces.is_some();
    let app_state = AppState {
        db: db.clone(),
        confirm_deadline_secs: args.confirm_deadline_secs,
        closure_upstream,
        rollouts_dir: args.rollouts_dir.clone(),
        rollouts_source: args.rollouts_source.clone(),
        channel_refs_source: args.channel_refs.clone(),
        strict: args.strict,
        agent_cn_suffix: args.agent_cn_suffix.clone(),
        agent_cert_validity: args.agent_cert_validity,
        revocations_required,
        bootstrap_nonces_required,
        ..Default::default()
    };
    if args.mark_ready_at_startup {
        // Test-only escape hatch - see ServeArgs::mark_ready_at_startup.
        app_state
            .artifact_primed
            .store(true, std::sync::atomic::Ordering::Release);
        app_state
            .revocations_primed
            .store(true, std::sync::atomic::Ordering::Release);
        app_state
            .bootstrap_nonces_primed
            .store(true, std::sync::atomic::Ordering::Release);
    }
    if let Some(nonces) = args.initial_nonces {
        // Test-only escape hatch - see ServeArgs::initial_nonces.
        *app_state.allowed_nonces.write().await = nonces;
    }
    let state = Arc::new(app_state);

    let cancel = CancellationToken::new();
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    if let Some(db_arc) = db.clone() {
        bg_handles.push(crate::timers::prune_timer::spawn(
            cancel.clone(),
            db_arc,
            args.db_path.clone(),
        ));
    }

    *state.issuance_paths.write().await = IssuancePaths {
        fleet_ca_cert: args.fleet_ca_cert.clone(),
        fleet_ca_key: args.fleet_ca_key.clone(),
        audit_log: args.audit_log_path.clone(),
        trust_path: args.trust_path.clone(),
    };

    // CA signer built once; TPM (pubkey + wrapper) wins over file (fleet_ca_key).
    if let Some(cert_path) = args.fleet_ca_cert.as_ref() {
        let signer = crate::auth::issuance::build_signer_from_args(
            cert_path,
            args.tpm_ca_pubkey_raw.as_deref(),
            args.tpm_ca_sign_wrapper.as_deref(),
            args.fleet_ca_key.as_deref(),
        );
        *state.ca_signer.write().await = signer;
    }

    // Pre-listener prime via manifest_poll. Synchronous one-shot fetch +
    // verify so the routes that read `state.verified_fleet` (channel_status,
    // enrollment) see a populated snapshot on the first request. After
    // this, the manifest_poll worker (spawned by runtime::spawn below)
    // keeps the snapshot fresh on its 30 s tick.
    {
        let clock: nixfleet_proto::clock::ClockHandle =
            std::sync::Arc::new(nixfleet_proto::clock::SystemClock::new());
        match tokio::time::timeout(
            Duration::from_secs(20),
            crate::runtime::workers::manifest_poll::prime_blocking(&state, &clock),
        )
        .await
        {
            Ok(Ok(true)) => {
                tracing::info!(
                    target: "cp_boot",
                    "primed verified-fleet from channel-refs source before opening listener",
                );
            }
            Ok(Ok(false)) => {
                // No channel_refs_source configured — fall through to file
                // fallback below.
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "manifest_poll prime failed; daemon will keep retrying via the worker loop",
                );
            }
            Err(_) => {
                tracing::warn!(
                    "manifest_poll prime timed out; daemon will keep retrying via the worker loop",
                );
            }
        }
    }

    // File-fallback prime: when no upstream channel-refs source is
    // configured (or its prime failed), verify the bundled --artifact /
    // --signature pair and populate verified_fleet. Keeps the offline-boot
    // + test fixture paths alive.
    if state.verified_fleet.read().await.is_none()
        && let Some((fleet, fleet_hash, artifact_bytes, signature_bytes)) =
            prime_from_artifact_files(
                &args.artifact_path,
                &args.signature_path,
                &args.trust_path,
                args.freshness_window,
            )
    {
        *state.verified_fleet.write().await = Some(VerifiedFleetSnapshot {
            fleet: Arc::new(fleet),
            fleet_resolved_hash: fleet_hash,
            artifact_bytes,
            signature_bytes,
        });
        state
            .artifact_primed
            .store(true, std::sync::atomic::Ordering::Release);
        tracing::info!(
            target: "cp_boot",
            "primed verified-fleet from --artifact / --signature files",
        );
    }

    if let (Some(revocations_source), Some(db)) = (args.revocations.clone(), state.db.clone()) {
        bg_handles.push(crate::polling::revocations_poll::spawn(
            cancel.clone(),
            db,
            revocations_source,
            state.revocations_primed.clone(),
        ));
    }

    if let Some(bootstrap_nonces_source) = args.bootstrap_nonces.clone() {
        bg_handles.push(crate::polling::bootstrap_nonces_poll::spawn(
            cancel.clone(),
            state.allowed_nonces.clone(),
            bootstrap_nonces_source,
            state.bootstrap_nonces_primed.clone(),
        ));
    }

    // RFC-0006 §7.2 runtime: MPSC reducer + applier + workers + event_log
    // writer. Sole CP-side processing path; the v0.1 reconcile loop is
    // gone.
    {
        let clock: nixfleet_proto::clock::ClockHandle =
            std::sync::Arc::new(nixfleet_proto::clock::SystemClock::new());
        let rt = crate::runtime::spawn(cancel.clone(), state.clone(), clock);
        // Publish channels for the new /v1/agent/{events,heartbeat,dispatch}
        // route handlers. `set` returns Err if already set; that's
        // impossible during normal startup (called exactly once) but if a
        // future test harness double-initialises, we just drop the second
        // call.
        let _ = state.runtime_input_tx.set(rt.input_tx.clone());
        let _ = state.runtime_event_log_tx.set(rt.event_log_tx.clone());
        for handle in rt.into_join_handles() {
            bg_handles.push(handle);
        }
    }

    // Process-global Prometheus recorder. Installs once; counter macros
    // (record_compliance_event etc) silently no-op until then.
    #[cfg(feature = "metrics")]
    crate::metrics::install_recorder();

    let app = build_router(state.clone());

    let tls_config =
        crate::tls::build_server_config(&args.tls_cert, &args.tls_key, args.client_ca.as_deref())?;
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

    let rustls_acceptor = axum_server::tls_rustls::RustlsAcceptor::new(rustls_config);
    let mtls_acceptor = MtlsAcceptor::new(rustls_acceptor);

    let mode = if args.client_ca.is_some() {
        "TLS+mTLS"
    } else {
        tracing::warn!(
            "control plane started without --client-ca: /v1/* endpoints will reject all clients with 401. \
             Pass --client-ca to enable mTLS - recommended for any production deployment."
        );
        "TLS-only"
    };
    let ready = state.is_ready();
    tracing::info!(
        listen = %args.listen,
        %mode,
        ready,
        "control plane listening{}",
        if ready { "" } else { "; /v1/* will return 503 until first artifact verified" },
    );

    let server_handle = axum_server::Handle::new();
    let signal_handle = server_handle.clone();
    let signal_cancel = cancel.clone();
    // LOADBEARING: graceful_shutdown FIRST (drains in-flight HTTP), THEN
    // cancel the token (signals background tasks). Reversing causes timers
    // to abort mid-write while requests are still completing.
    tokio::spawn(async move {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "ctrl_c handler install failed; relying on hard shutdown");
            return;
        }
        tracing::info!(target: "shutdown", "graceful shutdown initiated");
        signal_handle.graceful_shutdown(Some(HTTP_DRAIN_DEADLINE));
        signal_cancel.cancel();
    });

    axum_server::bind(args.listen)
        .acceptor(mtls_acceptor)
        .handle(server_handle)
        .serve(app.into_make_service())
        .await?;

    cancel.cancel();
    if let Err(err) = drain_background_tasks(bg_handles).await {
        tracing::warn!(error = %err, "background task drain incomplete");
    }
    Ok(())
}

/// File-fallback verify+parse for the --artifact / --signature CLI args.
/// Synchronous; called once at startup before the listener opens. Hash is
/// computed against the received bytes (not a re-serialised parsed struct)
/// so additive schema changes the CP's proto doesn't yet know about don't
/// shift the rolloutId anchor — same load-bearing reason as in
/// `manifest_poll::poll_once`.
fn prime_from_artifact_files(
    artifact_path: &std::path::Path,
    signature_path: &std::path::Path,
    trust_path: &std::path::Path,
    freshness_window: Duration,
) -> Option<(nixfleet_proto::FleetResolved, String, Vec<u8>, Vec<u8>)> {
    let artifact = std::fs::read(artifact_path).ok()?;
    let signature = std::fs::read(signature_path).ok()?;
    let trust_raw = std::fs::read_to_string(trust_path).ok()?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).ok()?;
    let now = chrono::Utc::now();
    let trusted_keys = trust.ci_release_key.active_keys_at(now);
    let reject_before = trust.ci_release_key.reject_before;
    let verified = nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trusted_keys,
        now,
        freshness_window,
        reject_before,
    )
    .ok()?;
    let fleet_hash = nixfleet_reconciler::canonical_hash_from_bytes(&artifact).ok()?;
    Some((verified.into_inner(), fleet_hash, artifact, signature))
}

/// Tasks past `TASK_SHUTDOWN_DEADLINE` are abandoned (handles dropped -> abort).
async fn drain_background_tasks(handles: Vec<tokio::task::JoinHandle<()>>) -> anyhow::Result<()> {
    let total = handles.len();
    let drain_fut = async move {
        for handle in handles {
            if let Err(err) = handle.await
                && !err.is_cancelled()
            {
                tracing::warn!(error = %err, "background task panicked during shutdown");
            }
        }
    };
    match tokio::time::timeout(TASK_SHUTDOWN_DEADLINE, drain_fut).await {
        Ok(()) => {
            tracing::info!(target: "shutdown", tasks = total, "all background tasks shut down");
            Ok(())
        }
        Err(_) => {
            anyhow::bail!(
                "background task drain exceeded {TASK_SHUTDOWN_DEADLINE:?}; forcing exit"
            );
        }
    }
}

#[cfg(test)]
mod strict_mode_tests {
    use super::*;
    use std::path::PathBuf;

    fn minimal_serve_args(strict: bool, client_ca: Option<PathBuf>) -> ServeArgs {
        ServeArgs {
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            client_ca,
            artifact_path: PathBuf::from("/dev/null"),
            signature_path: PathBuf::from("/dev/null"),
            trust_path: PathBuf::from("/dev/null"),
            observed_path: PathBuf::from("/dev/null"),
            strict,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn strict_bails_when_client_ca_unset() {
        let err = serve(minimal_serve_args(true, None)).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--client-ca"),
            "expected client-ca hint in strict bail; got: {msg}",
        );
        assert!(
            msg.contains("--strict refuses to start"),
            "expected strict-prefixed message; got: {msg}",
        );
    }

    #[tokio::test]
    async fn strict_bails_when_revocations_unset() {
        let err = serve(minimal_serve_args(true, Some(PathBuf::from("/dev/null"))))
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--revocations"),
            "expected revocations hint in strict bail; got: {msg}",
        );
    }

    #[tokio::test]
    async fn strict_bails_when_bootstrap_nonces_unset() {
        let mut args = minimal_serve_args(true, Some(PathBuf::from("/dev/null")));
        // Provide revocations so that only the bootstrap-nonces check fires.
        args.revocations = Some(crate::polling::revocations_poll::RevocationsSource {
            artifact_url: "http://localhost/revocations.json".into(),
            signature_url: "http://localhost/revocations.json.sig".into(),
            token_file: None,
            trust_path: PathBuf::from("/dev/null"),
            freshness_window: std::time::Duration::from_secs(3600),
        });
        let err = serve(args).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--bootstrap-nonces"),
            "expected bootstrap-nonces hint in strict bail; got: {msg}",
        );
        assert!(
            msg.contains("--strict refuses to start"),
            "expected strict-prefixed message; got: {msg}",
        );
    }

    #[tokio::test]
    async fn non_strict_does_not_bail_at_startup() {
        let err = serve(minimal_serve_args(false, None)).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            !msg.contains("--strict refuses to start"),
            "non-strict mode should not emit the strict-mode error; got: {msg}",
        );
    }

    /// Fixture: minimal args that satisfy every existing strict gate
    /// EXCEPT the CA backend gate. Lets each CA test isolate its assertion.
    fn args_satisfying_existing_strict_gates(strict: bool) -> ServeArgs {
        let mut args = minimal_serve_args(strict, Some(PathBuf::from("/dev/null")));
        args.revocations = Some(crate::polling::revocations_poll::RevocationsSource {
            artifact_url: "http://localhost/revocations.json".into(),
            signature_url: "http://localhost/revocations.json.sig".into(),
            token_file: None,
            trust_path: PathBuf::from("/dev/null"),
            freshness_window: std::time::Duration::from_secs(3600),
        });
        args.bootstrap_nonces = Some(
            crate::polling::bootstrap_nonces_poll::BootstrapNoncesSource {
                artifact_url: "http://localhost/bootstrap-nonces.json".into(),
                signature_url: "http://localhost/bootstrap-nonces.json.sig".into(),
                token_file: None,
                trust_path: PathBuf::from("/dev/null"),
                freshness_window: std::time::Duration::from_secs(3600),
            },
        );
        args
    }

    /// RFC-0010 §1.5.1: `--strict` + file-only CA + no opt-in must refuse to start.
    #[tokio::test]
    async fn strict_bails_when_file_ca_only_without_opt_in() {
        let mut args = args_satisfying_existing_strict_gates(true);
        args.fleet_ca_key = Some(PathBuf::from("/dev/null"));
        let err = serve(args).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--tpm-ca-pubkey-raw"),
            "expected TPM hint in strict bail; got: {msg}",
        );
        assert!(
            msg.contains("RFC-0010 §1.5.1"),
            "expected RFC pointer in strict bail; got: {msg}",
        );
        assert!(
            msg.contains("--strict refuses to start"),
            "expected strict-prefixed message; got: {msg}",
        );
    }

    /// RFC-0010 §1.5.1: explicit opt-in lets file-only CA pass `--strict`.
    /// Other startup paths may still fail; this test only asserts the CA
    /// gate does NOT contribute to the bail message.
    #[tokio::test]
    async fn strict_passes_ca_gate_when_file_ca_with_opt_in() {
        let mut args = args_satisfying_existing_strict_gates(true);
        args.fleet_ca_key = Some(PathBuf::from("/dev/null"));
        args.allow_file_ca_key = true;
        let err = serve(args)
            .await
            .err()
            .map(|e| format!("{e}"))
            .unwrap_or_default();
        assert!(
            !err.contains("--tpm-ca-pubkey-raw"),
            "opt-in should bypass the CA gate; got: {err}",
        );
    }

    /// RFC-0010 §1.5.1: TPM backend satisfies the CA gate without opt-in.
    #[tokio::test]
    async fn strict_passes_ca_gate_when_tpm_configured() {
        let mut args = args_satisfying_existing_strict_gates(true);
        args.tpm_ca_pubkey_raw = Some(PathBuf::from("/dev/null"));
        args.tpm_ca_sign_wrapper = Some(PathBuf::from("/dev/null"));
        // fleet_ca_key intentionally None: TPM is the only backend.
        let err = serve(args)
            .await
            .err()
            .map(|e| format!("{e}"))
            .unwrap_or_default();
        assert!(
            !err.contains("--tpm-ca-pubkey-raw"),
            "TPM backend should satisfy the gate; got: {err}",
        );
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn drain_returns_ok_when_tasks_exit_promptly() {
        let cancel = CancellationToken::new();
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let c = cancel.clone();
                tokio::spawn(async move {
                    c.cancelled().await;
                })
            })
            .collect();

        cancel.cancel();
        drain_background_tasks(handles)
            .await
            .expect("tasks should drain cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn drain_bails_when_task_ignores_cancel() {
        let cancel = CancellationToken::new();
        let stuck = tokio::spawn(async {
            tokio::time::sleep(TASK_SHUTDOWN_DEADLINE + Duration::from_secs(60)).await;
        });
        let handles = vec![stuck];

        cancel.cancel();
        let drain = tokio::spawn(async move { drain_background_tasks(handles).await });
        tokio::time::advance(TASK_SHUTDOWN_DEADLINE + Duration::from_secs(1)).await;
        let err = drain.await.unwrap().unwrap_err();
        assert!(
            err.to_string().contains("forcing exit"),
            "expected force-exit message; got: {err}",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_token_unblocks_select_loop() {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => return,
                    _ = ticker.tick() => {}
                }
            }
        });

        tokio::task::yield_now().await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("task should exit on cancel within 5s")
            .expect("task should not panic");
    }
}
