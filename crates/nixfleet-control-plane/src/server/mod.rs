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
//! - `handlers` — `/healthz` + `/v1/*` route handlers
//! - `reconcile` — background reconcile loop (verifies the
//!   build-time artifact every 30s, projects checkins → reconciler
//!   actions, writes the fleet snapshot under a freshness gate)
//!
//! Originally this was one 1450-LOC file; split here for readability
//! and to keep each piece focused.

mod checkin_pipeline;
mod enrollment_handlers;
mod handlers;
mod middleware;
mod reconcile;
mod report_handler;
mod state;
mod status_handlers;

pub use state::{
    AppState, ClosureUpstream, HostCheckinRecord, IssuancePaths, ReportRecord, ServeArgs,
};

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request as HttpRequest;
use axum::middleware::Next;
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;

use crate::auth_cn::MtlsAcceptor;
use crate::TickInputs;

/// Build the axum router. `/healthz` lives outside the `/v1` namespace
/// so it doesn't go through the protocol-version middleware
/// (operator status probe should always reply, regardless of header
/// version drift). `/v1/*` is the agent-facing surface and gates on
/// the protocol version header.
fn build_router(state: Arc<AppState>) -> Router {
    let v1_routes = Router::new()
        .route("/v1/whoami", get(handlers::whoami))
        .route("/v1/agent/checkin", post(checkin_pipeline::checkin))
        .route("/v1/agent/report", post(report_handler::report))
        .route("/v1/agent/confirm", post(checkin_pipeline::confirm))
        .route("/v1/agent/closure/{hash}", get(status_handlers::closure_proxy))
        .route("/v1/enroll", post(enrollment_handlers::enroll))
        .route("/v1/agent/renew", post(enrollment_handlers::renew))
        .route("/v1/channels/{name}", get(status_handlers::channel_status))
        .route("/v1/hosts", get(status_handlers::hosts_status))
        .layer(axum::middleware::from_fn(version_layer));

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .merge(v1_routes)
        .with_state(state)
}

/// Thin adapter so the router only sees a free function. Forwards to
/// the protocol-version middleware in [`middleware`].
async fn version_layer(
    req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    middleware::protocol_version_middleware(req, next).await
}

/// Serve until interrupted. Builds the TLS config, opens the DB,
/// primes the verified-fleet snapshot from Forgejo (when configured),
/// starts the reconcile loop + the Forgejo poll task, binds the
/// listener, runs forever.
pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
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
        ..Default::default()
    };
    let state = Arc::new(app_state);

    // Magic-rollback timer + hourly prune sweep .
    if let Some(db_arc) = db.clone() {
        crate::rollback_timer::spawn(db_arc.clone());
        crate::prune_timer::spawn(db_arc);
    }

    // — hydrate the in-memory `host_reports` ring from
    // SQLite at startup. Without this, the wave-staging gate
    // silently unlocks any held wave promotion across CP restart
    // until each agent re-fires its compliance gate. The DB write
    // happens write-through in the report handler, so as long as
    // SQLite has the rows, the in-memory ring buffer can be
    // reconstructed.
    if let Some(db_arc) = db.clone() {
        match db_arc.host_reports_known_hostnames() {
            Ok(hostnames) => {
                let mut total = 0usize;
                let mut reports_w = state.host_reports.write().await;
                for hostname in &hostnames {
                    match db_arc.host_reports_recent_per_host(
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
                                        crate::evidence_verify::SignatureStatus,
                                    >(serde_json::Value::String(s))
                                    .ok()
                                });
                                let buf = reports_w
                                    .entry(hostname.clone())
                                    .or_default();
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
            crate::channel_refs_poll::prime_once(channel_refs_source),
        )
        .await
        {
            Ok(Ok(fleet)) => {
                *state.verified_fleet.write().await = Some(Arc::new(fleet));
                tracing::info!(
                    target: "reconcile",
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
                tracing::warn!(
                    "channel-refs prime timed out; falling back to build-time artifact",
                );
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
    reconcile::spawn_reconcile_loop(state.clone(), tick_inputs);

    // Channel-refs poll: refresh `verified_fleet` + `channel_refs_cache`
    // from the configured upstream URLs (closes the GitOps loop). Both
    // shared locks live on `AppState` as `Arc<RwLock<...>>`; the poll
    // task writes through them directly without a mirror task.
    if let Some(channel_refs_source) = args.channel_refs.clone() {
        crate::channel_refs_poll::spawn(
            state.channel_refs_cache.clone(),
            state.verified_fleet.clone(),
            channel_refs_source,
        );
    }

    // Revocations poll : refresh `cert_revocations` from a
    // signed sidecar artifact every 60s. Requires a configured DB
    // (the replay target); a None DB silently disables the poll.
    if let (Some(revocations_source), Some(db)) = (
        args.revocations.clone(),
        state.db.clone(),
    ) {
        crate::revocations_poll::spawn(db, revocations_source);
    }

    let app = build_router(state);

    let tls_config = crate::tls::build_server_config(
        &args.tls_cert,
        &args.tls_key,
        args.client_ca.as_deref(),
    )?;
    let rustls_config =
        axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

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
    axum_server::bind(args.listen)
        .acceptor(mtls_acceptor)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}
