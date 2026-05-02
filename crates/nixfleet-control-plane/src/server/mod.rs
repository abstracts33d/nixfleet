//! Long-running TLS server.
//!
//! axum router + axum-server TLS listener + internal reconcile loop
//! + Forgejo poll. The slim entry point — `serve ` and
//!   `build_router ` — is what `main.rs` calls; everything else lives
//!   in the submodules:
//!
//! - `state` — shared `AppState`, `ServeArgs`, helper types,
//!   constants
//! - `middleware` — `require_cn` (mTLS gate) + protocol-version
//!   middleware
//! - `routes` — noun-based route handlers (`enrollment`, `reports`,
//!   `rollouts`, `status`, `health`)
//! - `checkin_pipeline` — the multi-stage `/v1/agent/checkin` and
//!   `/v1/agent/confirm` decision pipeline
//! - `reconcile` — background reconcile loop (verifies the
//!   build-time artifact every 30s, projects checkins → reconciler
//!   actions, writes the fleet snapshot under a freshness gate)
//!
//! Originally this was one 1450-LOC file; split here for readability
//! and to keep each piece focused.

mod checkin_pipeline;
mod middleware;
mod reconcile;
mod routes;
mod state;

pub use state::{
    AppState, ClosureUpstream, HostCheckinRecord, IssuancePaths, ReportRecord, ServeArgs,
    VerifiedFleetSnapshot,
};

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request as HttpRequest;
use axum::middleware::Next;
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::auth::auth_cn::MtlsAcceptor;
use crate::TickInputs;

/// Total budget for the post-listener-drain task gather phase.
/// 30s is the SystemD default for `TimeoutStopSec=`; staying under it
/// means systemd's `kill --kill -TERM` doesn't escalate to SIGKILL
/// before `serve()` finishes draining.
const TASK_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(30);

/// Within `TASK_SHUTDOWN_DEADLINE`, give axum-server `n - 5s` to drain
/// in-flight HTTP requests. The remaining 5s is for the post-listener
/// background-task gather. Both are heuristic; the listener drain
/// dominates real shutdowns.
const HTTP_DRAIN_DEADLINE: Duration = Duration::from_secs(25);

/// Build the axum router. `/healthz` lives outside the `/v1` namespace
/// so it doesn't go through the protocol-version middleware
/// (operator status probe should always reply, regardless of header
/// version drift). `/v1/*` is the agent-facing surface and gates on
/// the protocol version header.
fn build_router(state: Arc<AppState>) -> Router {
    let strict = state.strict;
    let v1_routes = Router::new()
        .route("/v1/whoami", get(routes::status::whoami))
        .route("/v1/agent/checkin", post(checkin_pipeline::checkin))
        .route("/v1/agent/report", post(routes::reports::report))
        .route("/v1/agent/confirm", post(checkin_pipeline::confirm))
        .route(
            "/v1/agent/closure/{hash}",
            get(routes::status::closure_proxy),
        )
        .route("/v1/enroll", post(routes::enrollment::enroll))
        .route("/v1/agent/renew", post(routes::enrollment::renew))
        .route("/v1/channels/{name}", get(routes::status::channel_status))
        .route("/v1/hosts", get(routes::status::hosts_status))
        .route("/v1/rollouts/{rolloutId}", get(routes::rollouts::manifest))
        .route(
            "/v1/rollouts/{rolloutId}/sig",
            get(routes::rollouts::signature),
        )
        .layer(axum::middleware::from_fn(move |req, next| {
            version_layer(strict, req, next)
        }));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .merge(v1_routes)
        .with_state(state)
}

/// Thin adapter so the router only sees a free function. Forwards to
/// the protocol-version middleware in [`middleware`].
async fn version_layer(
    strict: bool,
    req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    middleware::protocol_version_middleware(strict, req, next).await
}

/// Serve until interrupted. Builds the TLS config, opens the DB,
/// primes the verified-fleet snapshot from Forgejo (when configured),
/// starts the reconcile loop + the Forgejo poll task, binds the
/// listener, runs forever.
pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    // Strict mode: refuse to start when any security-fallback flag is
    // unset. Operator opts in via `--strict` / `NIXFLEET_CP_STRICT=1`.
    // Default off for v0.2 — see #70 for the rationale.
    if args.strict {
        let mut missing: Vec<&str> = Vec::new();
        if args.client_ca.is_none() {
            missing.push("--client-ca (mTLS verification disabled — TLS-only mode)");
        }
        if args.revocations.is_none() {
            missing.push("--revocations-{artifact,signature}-url (revocations polling disabled — previously-revoked certs become valid again after CP rebuild)");
        }
        if !missing.is_empty() {
            anyhow::bail!(
                "--strict refuses to start: the following security flags are unset:\n  - {}\n\
                 Either set the missing flags or drop --strict for development.",
                missing.join("\n  - "),
            );
        }
    }

    // Open + migrate SQLite if a path is configured.
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
    let app_state = AppState {
        db: db.clone(),
        confirm_deadline_secs: args.confirm_deadline_secs,
        closure_upstream,
        rollouts_dir: args.rollouts_dir.clone(),
        rollouts_source: args.rollouts_source.clone(),
        strict: args.strict,
        ..Default::default()
    };
    let state = Arc::new(app_state);

    // Root cancellation token. Every background loop selects against
    // this; SIGTERM cancels it after the HTTP drain completes. The
    // tasks log a "task X shut down" line on cancel, retained in the
    // Vec<JoinHandle> below so the shutdown phase can `join` them.
    let cancel = CancellationToken::new();
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Magic-rollback timer + hourly prune sweep .
    if let Some(db_arc) = db.clone() {
        bg_handles.push(crate::timers::rollback_timer::spawn(
            cancel.clone(),
            db_arc.clone(),
        ));
        bg_handles.push(crate::timers::prune_timer::spawn(
            cancel.clone(),
            db_arc,
            args.db_path.clone(),
        ));
    }

    // — hydrate the in-memory `host_reports` ring from
    // SQLite at startup. Without this, the wave-staging gate
    // silently unlocks any held wave promotion across CP restart
    // until each agent re-fires its compliance gate. The DB write
    // happens write-through in the report handler, so as long as
    // SQLite has the rows, the in-memory ring buffer can be
    // reconstructed.
    if let Some(db_arc) = db.clone() {
        match db_arc.reports().host_reports_known_hostnames() {
            Ok(hostnames) => {
                let mut total = 0usize;
                let mut reports_w = state.host_reports.write().await;
                for hostname in &hostnames {
                    match db_arc.reports().host_reports_recent_per_host(
                        hostname,
                        crate::server::state::REPORT_RING_CAP,
                    ) {
                        Ok(rows) => {
                            for row in rows {
                                let req: nixfleet_proto::agent_wire::ReportRequest =
                                    match serde_json::from_str(&row.report_json) {
                                        Ok(r) => r,
                                        Err(err) => {
                                            tracing::warn!(
                                                hostname = %hostname,
                                                event_id = %row.event_id,
                                                error = %err,
                                                "host_reports hydration: unparseable row, skipping"
                                            );
                                            continue;
                                        }
                                    };
                                let signature_status = row.signature_status.and_then(|s| {
                                    serde_json::from_value::<
                                        nixfleet_reconciler::evidence::SignatureStatus,
                                    >(serde_json::Value::String(
                                        s,
                                    ))
                                    .ok()
                                });
                                let buf = reports_w.entry(hostname.clone()).or_default();
                                buf.push_back(crate::server::ReportRecord {
                                    event_id: row.event_id,
                                    received_at: row.received_at,
                                    report: req,
                                    signature_status,
                                });
                                total += 1;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                hostname = %hostname,
                                error = %err,
                                "host_reports hydration: per-host query failed",
                            );
                        }
                    }
                }
                tracing::info!(
                    target: "boot",
                    hosts = hostnames.len(),
                    rows_loaded = total,
                    "host_reports hydration complete",
                );
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "host_reports hydration: failed to enumerate hostnames; ring buffer starts empty",
                );
            }
        }
    }

    // Seed issuance config.
    *state.issuance_paths.write().await = IssuancePaths {
        fleet_ca_cert: args.fleet_ca_cert.clone(),
        fleet_ca_key: args.fleet_ca_key.clone(),
        audit_log: args.audit_log_path.clone(),
    };

    // Pre-listener Forgejo prime: fetch the freshest signed artifact
    // from the operator's repo and seed `verified_fleet` BEFORE the
    // listener accepts the first checkin. Without this, dispatch
    // falls back to the compile-time `--artifact` path, which is
    // always an *older* release than what's on Forgejo (CI commits
    // the [skip ci] release commit AFTER building the closure — every
    // closure's bundled artifact is the previous release). Lab caught
    // this empirically during the GitOps validation pass: agents
    // checked in immediately on CP boot, before the periodic poll's
    // first tick, and dispatch issued stair-stepping-backwards
    // targets.
    //
    // Skip silently when no channel-refs source is configured or the
    // fetch fails — the reconcile loop's existing build-time prime is
    // the correct fallback. Cap with a short timeout so a wedged
    // upstream can't block CP boot indefinitely.
    if let Some(channel_refs_source) = args.channel_refs.as_ref() {
        match tokio::time::timeout(
            Duration::from_secs(20),
            crate::polling::channel_refs_poll::prime_once(channel_refs_source),
        )
        .await
        {
            Ok(Ok((fleet, fleet_hash))) => {
                // Host-count log line: ADR-012 documents the
                // Mutex<Connection> SQLite bound at ~150 hosts;
                // emitting the count at startup lets operators see
                // the curve in the journal without parsing the DB.
                let host_count = fleet.hosts.len();
                *state.verified_fleet.write().await =
                    Some(crate::server::VerifiedFleetSnapshot {
                        fleet: Arc::new(fleet),
                        fleet_resolved_hash: fleet_hash,
                    });
                tracing::info!(
                    target: "reconcile",
                    host_count,
                    "primed verified-fleet from channel-refs source before opening listener",
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "channel-refs prime failed; falling back to build-time artifact",
                );
            }
            Err(_) => {
                tracing::warn!("channel-refs prime timed out; falling back to build-time artifact",);
            }
        }
    }

    // Reconcile loop runs concurrently with the listener.
    let tick_inputs = TickInputs {
        artifact_path: args.artifact_path.clone(),
        signature_path: args.signature_path.clone(),
        trust_path: args.trust_path.clone(),
        observed_path: args.observed_path.clone(),
        now: Utc::now(),
        freshness_window: args.freshness_window,
    };
    bg_handles.push(reconcile::spawn_reconcile_loop(
        cancel.clone(),
        state.clone(),
        tick_inputs,
    ));

    // Channel-refs poll: refresh `verified_fleet` + `channel_refs_cache`
    // from the configured upstream URLs (closes the GitOps loop). Both
    // shared locks live on `AppState` as `Arc<RwLock<...>>`; the poll
    // task writes through them directly without a mirror task.
    if let Some(channel_refs_source) = args.channel_refs.clone() {
        bg_handles.push(crate::polling::channel_refs_poll::spawn(
            cancel.clone(),
            state.channel_refs_cache.clone(),
            state.verified_fleet.clone(),
            channel_refs_source,
        ));
    }

    // Revocations poll : refresh `cert_revocations` from a
    // signed sidecar artifact every 60s. Requires a configured DB
    // (the replay target); a None DB silently disables the poll.
    if let (Some(revocations_source), Some(db)) = (args.revocations.clone(), state.db.clone()) {
        bg_handles.push(crate::polling::revocations_poll::spawn(
            cancel.clone(),
            db,
            revocations_source,
        ));
    }

    let app = build_router(state);

    let tls_config =
        crate::tls::build_server_config(&args.tls_cert, &args.tls_key, args.client_ca.as_deref())?;
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

    // Wrap RustlsAcceptor in MtlsAcceptor so peer certs are
    // extracted after the handshake and injected into request
    // extensions. Handlers use `require_cn` to read them.
    let rustls_acceptor = axum_server::tls_rustls::RustlsAcceptor::new(rustls_config);
    let mtls_acceptor = MtlsAcceptor::new(rustls_acceptor);

    let mode = if args.client_ca.is_some() {
        "TLS+mTLS"
    } else {
        tracing::warn!(
            "control plane started without --client-ca: /v1/* endpoints will reject all clients with 401. \
             Pass --client-ca to enable mTLS — recommended for any production deployment."
        );
        "TLS-only"
    };
    tracing::info!(listen = %args.listen, %mode, "control plane listening");

    // axum-server Handle drives the listener-drain phase. ctrl_c
    // (SIGTERM under nix) signals graceful_shutdown, which stops
    // accepting new connections and lets in-flight requests complete
    // before returning from .serve().await.
    let server_handle = axum_server::Handle::new();
    let signal_handle = server_handle.clone();
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "ctrl_c handler install failed; relying on hard shutdown");
            return;
        }
        tracing::info!(target: "shutdown", "graceful shutdown initiated");
        // Drain in-flight HTTP first; once the listener is closed we
        // cancel the background loops so they finish their current
        // tick (DB writes complete) and exit.
        signal_handle.graceful_shutdown(Some(HTTP_DRAIN_DEADLINE));
        signal_cancel.cancel();
    });

    axum_server::bind(args.listen)
        .acceptor(mtls_acceptor)
        .handle(server_handle)
        .serve(app.into_make_service())
        .await?;

    // Listener has drained. Cancel background tasks (idempotent — the
    // ctrl_c handler may have fired already) and gather them under a
    // bounded budget so a stuck task can't wedge the shutdown.
    cancel.cancel();
    if let Err(err) = drain_background_tasks(bg_handles).await {
        tracing::warn!(error = %err, "background task drain incomplete");
    }
    Ok(())
}

/// Wait for every spawned background task to complete after cancel
/// has fired. Any task that doesn't return within [`TASK_SHUTDOWN_DEADLINE`]
/// is logged + abandoned (the JoinHandle is dropped, forcing abort).
async fn drain_background_tasks(
    handles: Vec<tokio::task::JoinHandle<()>>,
) -> anyhow::Result<()> {
    let total = handles.len();
    let drain_fut = async move {
        for handle in handles {
            if let Err(err) = handle.await {
                if !err.is_cancelled() {
                    tracing::warn!(error = %err, "background task panicked during shutdown");
                }
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
        // client_ca provided, but no revocations → strict still bails.
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
    async fn non_strict_does_not_bail_at_startup() {
        // strict=false, client_ca=None → must NOT bail with the
        // strict-mode error. Will still error later (the TLS cert at
        // /dev/null isn't a real cert), but that error is downstream
        // of the strict check we're testing.
        let err = serve(minimal_serve_args(false, None)).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            !msg.contains("--strict refuses to start"),
            "non-strict mode should not emit the strict-mode error; got: {msg}",
        );
    }
}

#[cfg(test)]
mod shutdown_tests {
    //! Unit tests for the graceful-shutdown plumbing in serve().
    //! Full serve() integration is covered by the existing
    //! tests/healthz.rs etc. (each spawns serve() then aborts the
    //! handle — the abort path is unchanged by these changes); these
    //! tests exercise the building blocks directly.

    use super::*;
    use std::time::Duration;

    /// drain_background_tasks returns Ok when every task completes
    /// before the deadline. Tasks that exit on cancel.cancelled() are
    /// the standard shape for our background loops.
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

    /// drain_background_tasks bails when at least one task ignores the
    /// cancel signal and runs past the deadline. The deadline constant
    /// is 30s in production; the test uses tokio's pause/advance to
    /// fast-forward without real wall-clock waiting.
    #[tokio::test(start_paused = true)]
    async fn drain_bails_when_task_ignores_cancel() {
        let cancel = CancellationToken::new();
        // Task that ignores cancel and just sleeps past the deadline.
        let stuck = tokio::spawn(async {
            tokio::time::sleep(TASK_SHUTDOWN_DEADLINE + Duration::from_secs(60)).await;
        });
        let handles = vec![stuck];

        cancel.cancel();
        // Advance past TASK_SHUTDOWN_DEADLINE so the timeout fires.
        let drain = tokio::spawn(async move { drain_background_tasks(handles).await });
        tokio::time::advance(TASK_SHUTDOWN_DEADLINE + Duration::from_secs(1)).await;
        let err = drain.await.unwrap().unwrap_err();
        assert!(
            err.to_string().contains("forcing exit"),
            "expected force-exit message; got: {err}",
        );
    }

    /// Minimal sanity check that the cancel-token + select-arm pattern
    /// in our spawn_* helpers actually exits when cancel fires. Spawns
    /// a tokio::select loop with a long ticker and a cancel arm; the
    /// task must exit on cancel even though the ticker hasn't fired.
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

        // Give the spawned task one tick to enter its select loop.
        tokio::task::yield_now().await;
        cancel.cancel();
        // Bound the wait — if the cancel-arm doesn't unblock, the
        // task will sit on the 1h ticker and the test will hang.
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("task should exit on cancel within 5s")
            .expect("task should not panic");
    }
}
