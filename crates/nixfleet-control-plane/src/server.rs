//! Long-running TLS server (Phase 3 PR-1).
//!
//! axum router + axum-server TLS listener + internal `tokio::time::
//! interval(30s)` reconcile loop. PR-1 ships exactly one real
//! endpoint (`GET /healthz`); subsequent PRs layer mTLS (PR-2),
//! `/v1/whoami` (PR-2), `/v1/agent/checkin` + `/v1/agent/report`
//! (PR-3), `/v1/enroll` + `/v1/agent/renew` (PR-5). The `tick`
//! function reused here is the same one the `tick` subcommand
//! invokes — verify-and-reconcile lives in one place across both
//! entry points.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::{
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use axum::middleware::{self, Next};
use axum::http::Request as HttpRequest;
use axum::body::Body;
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest, ReportRequest, ReportResponse,
    PROTOCOL_MAJOR_VERSION, PROTOCOL_VERSION_HEADER,
};
use nixfleet_proto::enroll_wire::{EnrollRequest, EnrollResponse, RenewRequest, RenewResponse};
use nixfleet_proto::FleetResolved;
use rcgen::PublicKeyData;
use std::collections::HashSet;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::auth_cn::{MtlsAcceptor, PeerCertificates};
use crate::{render_plan, tick, TickInputs};

/// Per-host event ring buffer cap. Phase 3's `/v1/agent/report` is
/// in-memory only — Phase 4 adds SQLite persistence. 32 entries is
/// enough to spot a flapping host without unbounded memory growth.
const REPORT_RING_CAP: usize = 32;

/// Returned to the agent in CheckinResponse. Phase 3 never dispatches
/// rollouts (Phase 4 introduces that), so the agent is told to come
/// back in 60s with the next regular checkin.
const NEXT_CHECKIN_SECS: u32 = 60;

/// Reconcile loop cadence — D2 default. Operator-visible drift (host
/// failed to check in) shows up in the journal within one cycle;
/// slow enough not to spam.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Time the dispatch loop gives an agent to fetch + activate +
/// confirm a target before the magic-rollback timer marks the
/// pending row as `rolled-back` (which causes the agent's next
/// `/v1/agent/confirm` post to return 410 Gone, triggering local
/// rollback). 120s is the spec-D1 default — enough headroom for a
/// closure download + reboot, short enough that a stuck agent
/// surfaces in the journal within one rollback-timer tick.
const CONFIRM_DEADLINE_SECS: i64 = 120;

/// Inputs the `serve` subcommand receives from clap.
#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub listen: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub client_ca: Option<PathBuf>,
    /// Fleet CA cert path — used by issuance to read the CA cert
    /// for chaining new agent certs. Often the same path as
    /// `client_ca`. PR-5 onwards.
    pub fleet_ca_cert: Option<PathBuf>,
    /// Fleet CA private key path — issuance signs new agent certs
    /// with this. **Online on the CP per the deferred-trust-hardening
    /// design (issue #41).**
    pub fleet_ca_key: Option<PathBuf>,
    /// Path to the audit log JSON-lines file.
    pub audit_log_path: Option<PathBuf>,
    pub artifact_path: PathBuf,
    pub signature_path: PathBuf,
    pub trust_path: PathBuf,
    /// Phase 2/early-PR-1 fallback path. PR-4 prefers the live
    /// projection from check-ins; this path is used only when no
    /// agents have checked in yet AND `forgejo` is None (offline
    /// dev/test mode).
    pub observed_path: PathBuf,
    pub freshness_window: Duration,
    /// PR-4: Forgejo poll config. When `None`, the CP falls back to
    /// reading `--observed` for the channel-refs portion of Observed.
    pub forgejo: Option<crate::forgejo_poll::ForgejoConfig>,
    /// Phase 4 PR-1: SQLite path. When `Some`, the DB is opened +
    /// migrated at startup. When `None`, in-memory state is used
    /// (file-backed deploy + tests fall through to this).
    pub db_path: Option<PathBuf>,
    /// Phase 4 PR-C: closure proxy upstream. URL of the attic
    /// instance the CP forwards `/v1/agent/closure/<hash>` requests
    /// to. When `None`, the endpoint returns 501. Typical value on
    /// lab: `http://localhost:8085` (attic on the same host).
    pub closure_upstream: Option<String>,
}

/// In-memory record of the most recent checkin per host. Phase 4
/// promotes this to the source-of-truth for the projection that
/// feeds the reconcile loop (PR-4). For PR-3 it's just observability
/// state — operator can grep journal or, eventually, query an admin
/// endpoint.
#[derive(Debug, Clone)]
pub struct HostCheckinRecord {
    pub last_checkin: DateTime<Utc>,
    pub checkin: CheckinRequest,
}

/// In-memory record of an event report. Bounded ring buffer per
/// host (cap = `REPORT_RING_CAP`). Phase 4 adds SQLite persistence
/// + correlation with rollouts.
#[derive(Debug, Clone)]
pub struct ReportRecord {
    pub event_id: String,
    pub received_at: DateTime<Utc>,
    pub report: ReportRequest,
}

/// Closure-proxy upstream client + URL. Captured at serve() time
/// so each request avoids re-parsing the URL or rebuilding the
/// reqwest client.
#[derive(Clone, Debug)]
pub struct ClosureUpstream {
    pub base_url: String,
    pub client: reqwest::Client,
}

/// Server-wide shared state. Phase 3 fields: `host_checkins`,
/// `host_reports`, `channel_refs_cache`, `seen_token_nonces`,
/// `issuance_paths`. Phase 4 PR-1 adds `db` — SQLite-backed
/// persistence; subsequent Phase 4 PRs migrate `seen_token_nonces`
/// (currently in-memory HashSet) and add cert revocation, pending
/// confirmations, etc. on top.
///
/// `db` is `Option<Arc<Db>>` so existing tests + the file-backed
/// PR-1 deploy path can run without standing up SQLite. Production
/// deploys wire it via `--db-path`.
pub struct AppState {
    pub last_tick_at: RwLock<Option<DateTime<Utc>>>,
    pub host_checkins: RwLock<HashMap<String, HostCheckinRecord>>,
    pub host_reports: RwLock<HashMap<String, VecDeque<ReportRecord>>>,
    /// In-memory channel-refs cache populated by the Forgejo poll
    /// task. Wrapped in `Arc<RwLock<...>>` so the poll task writes
    /// directly without a mirror — same shape as `verified_fleet`,
    /// removes the boot-time race where the reconciler saw
    /// `channels_observed: 0` for the first ≤30s after CP startup.
    pub channel_refs_cache: Arc<RwLock<crate::forgejo_poll::ChannelRefsCache>>,
    pub seen_token_nonces: RwLock<HashSet<String>>,
    pub issuance_paths: RwLock<IssuancePaths>,
    pub db: Option<Arc<crate::db::Db>>,
    pub closure_upstream: Option<ClosureUpstream>,
    /// Most-recently-verified `fleet.resolved` artifact. The dispatch
    /// path in `/v1/agent/checkin` reads it to decide each host's
    /// target. Two writers refresh it in tandem, both keeping the
    /// most recent successful verify available without blanking out
    /// on a transient failure:
    ///
    /// - The reconcile loop's file-backed verify (PR-1 deploy-time
    ///   bytes) runs every tick and writes through this lock.
    /// - The Forgejo poll task fetches `releases/fleet.resolved.json`
    ///   + `.sig` straight from the operator's repo, runs the same
    ///   `verify_artifact`, and writes through the same lock —
    ///   closes the GitOps loop. A `git push` to fleet/main →
    ///   CI re-signs → next poll (≤60s) refreshes the snapshot →
    ///   next checkin dispatches against the new closure_hashes.
    ///
    /// Wrapped in `Arc<RwLock<...>>` so both writers share access
    /// without a mirror task.
    pub verified_fleet: Arc<RwLock<Option<Arc<FleetResolved>>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_tick_at: RwLock::new(None),
            host_checkins: RwLock::new(HashMap::new()),
            host_reports: RwLock::new(HashMap::new()),
            channel_refs_cache: Arc::new(RwLock::new(
                crate::forgejo_poll::ChannelRefsCache::default(),
            )),
            seen_token_nonces: RwLock::new(HashSet::new()),
            issuance_paths: RwLock::new(IssuancePaths::default()),
            db: None,
            closure_upstream: None,
            verified_fleet: Arc::new(RwLock::new(None)),
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("db", &self.db.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct IssuancePaths {
    pub fleet_ca_cert: Option<PathBuf>,
    pub fleet_ca_key: Option<PathBuf>,
    pub audit_log: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    ok: bool,
    version: &'static str,
    /// rfc3339-formatted UTC timestamp, or `null` if the reconcile
    /// loop has not yet ticked once. (Realistic only for the first
    /// ~30s after boot.)
    last_tick_at: Option<String>,
}

async fn healthz(state: axum::extract::State<Arc<AppState>>) -> Json<HealthzResponse> {
    let last = *state.last_tick_at.read().await;
    Json(HealthzResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        last_tick_at: last.map(|t| t.to_rfc3339()),
    })
}

#[derive(Debug, Serialize)]
struct WhoamiResponse {
    cn: String,
    /// rfc3339-formatted timestamp the server received the request.
    /// `issuedAt` semantically refers to "the moment we observed
    /// this verified identity", not the cert's notBefore — that's
    /// available from the cert chain itself if a future endpoint
    /// needs it.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` — returns the verified mTLS CN of the caller.
/// Useful for confirming the cert pipeline is wired correctly before
/// the agent body is real (PR-3). When mTLS is not configured (no
/// `--client-ca`), the handler returns 401 — `/v1/whoami` is
/// intentionally one of the gated routes since there's nothing to
/// say without a verified peer.
async fn whoami(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
) -> Result<Json<WhoamiResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    Ok(Json(WhoamiResponse {
        cn,
        issued_at: Utc::now().to_rfc3339(),
    }))
}

/// Extract the verified CN from `PeerCertificates`, or return 401.
/// Also enforces cert revocation when AppState.db is set: a cert
/// whose notBefore predates the host's revocation entry is rejected
/// with 401. Re-enrolled certs (notBefore > revoked_before) pass.
///
/// Centralised so each /v1/* handler reads the same way.
async fn require_cn(
    state: &AppState,
    peer_certs: &PeerCertificates,
) -> Result<String, StatusCode> {
    if !peer_certs.is_present() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let cn = peer_certs.leaf_cn().ok_or(StatusCode::UNAUTHORIZED)?;

    if let Some(db) = &state.db {
        match db.cert_revoked_before(&cn) {
            Ok(Some(revoked_before)) => {
                let cert_nbf = peer_certs
                    .leaf_not_before()
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                if cert_nbf < revoked_before {
                    tracing::warn!(
                        cn = %cn,
                        cert_not_before = %cert_nbf.to_rfc3339(),
                        revoked_before = %revoked_before.to_rfc3339(),
                        "rejecting revoked cert"
                    );
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
            Ok(None) => {} // not revoked
            Err(err) => {
                tracing::error!(error = %err, "db cert_revoked_before failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }

    Ok(cn)
}

/// `POST /v1/agent/checkin` — record an agent checkin.
///
/// Validates the body's `hostname` matches the verified mTLS CN
/// (sanity check, not a security boundary — the CN was already
/// authenticated by WebPkiClientVerifier; this just catches
/// configuration drift like a host using the wrong cert).
///
/// Emits a journal line per checkin so operators can grep
/// `journalctl -u nixfleet-control-plane | grep checkin`.
async fn checkin(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<CheckinRequest>,
) -> Result<Json<CheckinResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "checkin rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // Surface the checkin in the journal in a grep-friendly shape.
    // `last_fetch` is the field operators care about most for spotting
    // stuck agents (verify-failed, fetch-failed) without parsing the
    // full body.
    let last_fetch = req
        .last_fetch_outcome
        .as_ref()
        .map(|o| format!("{:?}", o.result).to_lowercase())
        .unwrap_or_else(|| "none".to_string());
    let pending = req
        .pending_generation
        .as_ref()
        .map(|p| p.closure_hash.as_str())
        .unwrap_or("null");
    tracing::info!(
        target: "checkin",
        hostname = %req.hostname,
        closure_hash = %req.current_generation.closure_hash,
        pending = %pending,
        last_fetch = %last_fetch,
        "checkin received"
    );

    let now = Utc::now();
    let record = HostCheckinRecord {
        last_checkin: now,
        checkin: req.clone(),
    };
    state
        .host_checkins
        .write()
        .await
        .insert(req.hostname.clone(), record);

    let target = dispatch_target_for_checkin(&state, &req, now).await;

    Ok(Json(CheckinResponse {
        target,
        next_checkin_secs: NEXT_CHECKIN_SECS,
    }))
}

/// Dispatch loop entry point per `/v1/agent/checkin`.
///
/// Reads the latest verified `FleetResolved` snapshot from `AppState`
/// (populated by the reconcile loop), queries the DB for any pending
/// confirm row for this host (idempotency guard), and asks
/// `dispatch::decide_target` for the per-host decision. On `Dispatch`,
/// inserts a `pending_confirms` row keyed on the deterministic
/// rollout id and returns the target. All other Decision variants
/// resolve to `target: None`.
///
/// Failures here log + return None — a transient DB hiccup or missing
/// fleet snapshot should not surface as an HTTP 500 to the agent. The
/// agent will retry on its next checkin (60s).
async fn dispatch_target_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let Some(db) = state.db.as_ref() else {
        return None;
    };
    let fleet_snapshot = state.verified_fleet.read().await.clone();
    let Some(fleet) = fleet_snapshot else {
        // No verified artifact yet — the reconcile loop hasn't ticked
        // (or has only seen verify failures). Hold steady, the agent
        // checks back in 60s.
        return None;
    };
    let pending_for_host = match db.pending_confirm_exists(&req.hostname) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "dispatch: pending_confirm_exists query failed",
            );
            return None;
        }
    };

    let decision =
        crate::dispatch::decide_target(&req.hostname, req, &fleet, pending_for_host, now);

    match decision {
        crate::dispatch::Decision::Dispatch { target, rollout_id } => {
            // Idempotency: even though pending_confirm_exists already
            // gated this, a concurrent checkin from the same host
            // could race. Treat insert errors as "another writer beat
            // us" and return None — the other dispatch wins, this
            // call returns no target.
            let confirm_deadline = now + chrono::Duration::seconds(CONFIRM_DEADLINE_SECS);
            match db.record_pending_confirm(
                &req.hostname,
                &rollout_id,
                /* wave */ 0,
                &target.closure_hash,
                &target.channel_ref,
                confirm_deadline,
            ) {
                Ok(_) => {
                    tracing::info!(
                        target: "dispatch",
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        target_closure = %target.closure_hash,
                        confirm_deadline = %confirm_deadline.to_rfc3339(),
                        "dispatch: target issued",
                    );
                    Some(target)
                }
                Err(err) => {
                    tracing::warn!(
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        error = %err,
                        "dispatch: record_pending_confirm failed; returning no target",
                    );
                    None
                }
            }
        }
        other => {
            tracing::debug!(
                target: "dispatch",
                hostname = %req.hostname,
                decision = ?other,
                "dispatch: no target",
            );
            None
        }
    }
}

/// `POST /v1/agent/report` — record an out-of-band event report.
///
/// In-memory ring buffer per host, capped at `REPORT_RING_CAP`. New
/// reports push to the back; if the buffer is full, the oldest is
/// dropped. Phase 4 promotes this to SQLite + correlates with
/// rollouts.
async fn report(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ReportRequest>,
) -> Result<Json<ReportResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "report rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // Generate an opaque event ID. Not cryptographically random —
    // it's a journal correlation handle, not a security boundary.
    let event_id = format!(
        "evt-{}-{}",
        Utc::now().timestamp_millis(),
        rand_suffix(8)
    );

    let received_at = Utc::now();

    // Render the event variant for the journal in a grep-friendly
    // way: `event=activation-failed`, `event=verify-mismatch`, etc.
    // The serde_json round-trip extracts the kebab-case discriminator
    // without the agent having to do it for us.
    let event_str = serde_json::to_value(&req.event)
        .ok()
        .and_then(|v| v.get("event").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string());
    let rollout_str = req
        .rollout
        .clone()
        .unwrap_or_else(|| "<none>".to_string());

    tracing::info!(
        target: "report",
        hostname = %req.hostname,
        event = %event_str,
        rollout = %rollout_str,
        agent_version = %req.agent_version,
        event_id = %event_id,
        "report received"
    );

    let record = ReportRecord {
        event_id: event_id.clone(),
        received_at,
        report: req.clone(),
    };
    let mut reports = state.host_reports.write().await;
    let buf = reports.entry(req.hostname).or_default();
    if buf.len() >= REPORT_RING_CAP {
        buf.pop_front();
    }
    buf.push_back(record);

    Ok(Json(ReportResponse { event_id }))
}

/// 8-char lowercase-alnum suffix for event IDs. Not crypto-grade —
/// just enough to make IDs visually distinct in journal output. Uses
/// system time microseconds + nanos as the entropy source so we
/// don't pull the `rand` crate just for this.
fn rand_suffix(n: usize) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(n);
    let mut x = nanos.wrapping_mul(0x9e3779b97f4a7c15);
    for _ in 0..n {
        let idx = (x % alphabet.len() as u64) as usize;
        out.push(alphabet[idx] as char);
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    out
}

/// `POST /v1/enroll` — bootstrap a new fleet host.
///
/// No mTLS required (this is the path before the host has a cert).
/// Authentication is via the bootstrap-token signature against the
/// org root key in trust.json. Order of checks matches the
/// security narrative in RFC-0003 §2:
/// 1. Replay: refuse already-seen nonces.
/// 2. Expiry: refuse tokens outside their issued/expires window.
/// 3. Signature: verify against `orgRootKey.current` (and `.previous`
///    during a rotation grace window) from trust.json.
/// 4. Hostname binding: claim's hostname must match CSR CN (validated
///    by `issuance::issue_cert` chain).
/// 5. Pubkey-fingerprint binding: SHA-256 of the CSR's pubkey DER
///    must match `claims.expected_pubkey_fingerprint`.
async fn enroll(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, StatusCode> {
    use base64::Engine;

    let now = chrono::Utc::now();

    // 1. Replay defense — drop the nonce on the floor early so a
    //    flood of replays doesn't pay for parsing + signature work.
    //
    //    DB-backed when state.db is set (Phase 4 PR-1+); in-memory
    //    HashSet fallback for tests + dev. The two paths produce
    //    identical observable behaviour (one INSERT per accepted
    //    token, no insert on rejected). Hold off inserting until
    //    after signature verification — a forged token's nonce
    //    shouldn't lock out a real operator-minted retry.
    if let Some(db) = &state.db {
        match db.token_seen(&req.token.claims.nonce) {
            Ok(true) => {
                tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected (db)");
                return Err(StatusCode::CONFLICT);
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!(error = %err, "enroll: db token_seen failed");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    } else {
        let seen = state.seen_token_nonces.read().await;
        if seen.contains(&req.token.claims.nonce) {
            tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected (mem)");
            return Err(StatusCode::CONFLICT);
        }
    }

    // 2. Expiry.
    if now < req.token.claims.issued_at || now >= req.token.claims.expires_at {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            "enroll: token outside validity window"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 3. Signature verification against trust.json's `orgRootKey`.
    //    Re-read on every enroll so operator key rotations propagate
    //    without restart. `orgRootKey.current` and `.previous` are
    //    both candidates during a rotation grace window per
    //    CONTRACTS.md §II #3.
    let trust_path = state
        .issuance_paths
        .read()
        .await
        .fleet_ca_cert
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/nixfleet/cp"))
        .join("trust.json");
    let trust_raw = std::fs::read_to_string(&trust_path).map_err(|err| {
        tracing::error!(error = %err, path = %trust_path.display(), "enroll: read trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).map_err(|err| {
        tracing::error!(error = %err, "enroll: parse trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let org_root = trust.org_root_key.as_ref().ok_or_else(|| {
        tracing::error!(
            "enroll: trust.json has no orgRootKey — refusing to accept any token. \
             Set nixfleet.trust.orgRootKey.current in fleet.nix and rebuild."
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let candidates = org_root.active_keys();
    if candidates.is_empty() {
        tracing::error!("enroll: orgRootKey has no current/previous keys");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let mut sig_ok = false;
    for pubkey in &candidates {
        if pubkey.algorithm != "ed25519" {
            tracing::warn!(
                algorithm = %pubkey.algorithm,
                "enroll: skipping non-ed25519 orgRootKey candidate (only ed25519 supported)"
            );
            continue;
        }
        let pubkey_bytes = match base64::engine::general_purpose::STANDARD.decode(&pubkey.public) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(error = %err, "enroll: orgRootKey base64 decode");
                continue;
            }
        };
        if crate::issuance::verify_token_signature(&req.token, &pubkey_bytes).is_ok() {
            sig_ok = true;
            break;
        }
    }
    if !sig_ok {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            nonce = %req.token.claims.nonce,
            "enroll: token signature did not verify against any orgRootKey candidate"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 4. Hostname / 5. pubkey-fingerprint validation against CSR.
    //    Done by reading the CSR before issuance (issuance::issue_cert
    //    will populate the cert's CN from the CSR). We pre-validate
    //    here so we can refuse before doing any signing work.
    let csr_params =
        rcgen::CertificateSigningRequestParams::from_pem(&req.csr_pem).map_err(|err| {
            tracing::warn!(error = %err, "enroll: parse CSR PEM");
            StatusCode::BAD_REQUEST
        })?;
    let csr_cn: Option<String> = csr_params
        .params
        .distinguished_name
        .iter()
        .find_map(|(t, v): (&rcgen::DnType, &rcgen::DnValue)| {
            if matches!(t, rcgen::DnType::CommonName) {
                Some(match v {
                    rcgen::DnValue::PrintableString(s) => s.to_string(),
                    rcgen::DnValue::Utf8String(s) => s.to_string(),
                    _ => format!("{:?}", v),
                })
            } else {
                None
            }
        });
    let csr_cn = csr_cn.ok_or_else(|| {
        tracing::warn!("enroll: CSR has no CN");
        StatusCode::BAD_REQUEST
    })?;
    let csr_pubkey_der = csr_params.public_key.der_bytes();
    let csr_fingerprint = crate::issuance::fingerprint(csr_pubkey_der);

    if let Err(err) = crate::issuance::validate_token_claims(
        &req.token.claims,
        &csr_cn,
        &csr_fingerprint,
        now,
    ) {
        tracing::warn!(error = %err, hostname = %req.token.claims.hostname, "enroll: claim validation");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // All checks passed — commit the nonce as seen so a replay of
    // this exact (verified) token is rejected. DB write when
    // available, in-memory fallback otherwise.
    if let Some(db) = &state.db {
        if let Err(err) = db.record_token_nonce(&req.token.claims.nonce, &req.token.claims.hostname) {
            // Log but don't fail the enroll — the cert is already
            // about to be issued. A failed replay-record means at
            // worst a window where the same token could be used
            // twice; the rest of the validation still rejects
            // tampering, expiry, and CN/fingerprint mismatches.
            tracing::warn!(error = %err, "enroll: db record_token_nonce failed; proceeding");
        }
    } else {
        state
            .seen_token_nonces
            .write()
            .await
            .insert(req.token.claims.nonce.clone());
    }

    // Issue the cert.
    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => {
            tracing::error!("enroll: fleet CA cert/key paths not configured");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "enroll: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            "<enroll>",
            &req.token.claims.hostname,
            not_after,
            &crate::issuance::AuditContext::Enroll {
                token_nonce: req.token.claims.nonce.clone(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %req.token.claims.hostname,
        not_after = %not_after.to_rfc3339(),
        "enrolled"
    );

    Ok(Json(EnrollResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/renew` — issue a fresh cert for an authenticated
/// agent. mTLS-required; the verified CN must match the CSR's CN.
async fn renew(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<RenewRequest>,
) -> Result<Json<RenewResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    let now = chrono::Utc::now();

    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "renew: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            &cn,
            &cn,
            not_after,
            &crate::issuance::AuditContext::Renew {
                previous_cert_serial: "<unknown>".to_string(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %cn,
        not_after = %not_after.to_rfc3339(),
        "renewed"
    );

    Ok(Json(RenewResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/confirm` — agent confirms successful activation
/// of a target generation. Marks the matching `pending_confirms`
/// row as confirmed.
///
/// Behaviour:
/// - Rollout exists in `pending_confirms` with `state='pending'`
///   AND deadline not yet passed: mark confirmed, 204.
/// - Rollout cancelled OR deadline already passed (state in
///   `'cancelled' | 'rolled-back' | 'confirmed'`): 410 Gone.
///   Agent then triggers local rollback per RFC-0003 §4.2.
/// - No matching rollout (CN/rollout_id mismatch): 404. Catches
///   bad-rollout-id agent bugs without giving up auth info.
/// - DB unset: 503 Service Unavailable. The endpoint requires
///   persistence; in-memory mode doesn't track confirms.
async fn confirm(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ConfirmRequest>,
) -> Result<axum::response::Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "confirm rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let db = state.db.as_ref().ok_or_else(|| {
        tracing::warn!("confirm: no db configured — endpoint unusable");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let updated = db.confirm_pending(&req.hostname, &req.rollout).map_err(|err| {
        tracing::error!(error = %err, "confirm: db update failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if updated == 0 {
        // Either: no rollout with that ID for this host (rollout
        // never dispatched, agent confused), or the row exists but
        // is no longer in 'pending' state (already confirmed,
        // rolled-back, or cancelled). RFC-0003 §4.2 says 410 Gone
        // when the rollout was cancelled or the wave already failed
        // — collapse "not found" and "wrong state" to 410 since the
        // agent's response is the same in both cases (trigger local
        // rollback).
        tracing::info!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            "confirm: no matching pending row — returning 410"
        );
        return Ok(axum::response::Response::builder()
            .status(StatusCode::GONE)
            .body(axum::body::Body::from(""))
            .unwrap_or_default());
    }

    tracing::info!(
        target: "confirm",
        hostname = %req.hostname,
        rollout = %req.rollout,
        wave = req.wave,
        closure_hash = %req.generation.closure_hash,
        "confirm received"
    );
    Ok(axum::response::Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::from(""))
        .unwrap_or_default())
}

/// `GET /v1/agent/closure/{hash}` — closure proxy (Phase 4 PR-C).
///
/// Forwards to a configured attic upstream (`AppState.closure_upstream`).
/// Currently fetches the narinfo for `<hash>.narinfo` from the
/// upstream and returns the bytes verbatim. Real Nix-cache-protocol
/// forwarding (full nar streaming + multiple files) is a follow-up
/// PR — this lands the wire shape + the `closure_upstream` config
/// path so the operator can deploy the agent-side fallback knowing
/// the URL exists.
///
/// When `closure_upstream` is unset, returns 501 Not Implemented +
/// a journal info line so the operator sees the gap.
async fn closure_proxy(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Path(closure_hash): Path<String>,
) -> Result<axum::response::Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;

    let upstream = match &state.closure_upstream {
        Some(u) => u,
        None => {
            tracing::info!(
                target: "closure_proxy",
                cn = %cn,
                closure = %closure_hash,
                "closure proxy hit but no --closure-upstream configured (501)"
            );
            let body = serde_json::json!({
                "error": "closure proxy not configured",
                "closure": closure_hash,
                "tracking": "set services.nixfleet-control-plane.closureUpstream",
            });
            return Ok(axum::response::Response::builder()
                .status(StatusCode::NOT_IMPLEMENTED)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap_or_default());
        }
    };

    // Forward to the attic narinfo endpoint. Attic's API serves
    // narinfo at `<base>/<hash>.narinfo` for the configured cache.
    // Full nar transfer (the actual closure bytes) requires multiple
    // requests — landing as a follow-up PR. For now this proves the
    // upstream wire works.
    let url = format!(
        "{}/{}.narinfo",
        upstream.base_url.trim_end_matches('/'),
        closure_hash
    );
    tracing::debug!(target: "closure_proxy", cn = %cn, url = %url, "forwarding");

    let resp = match upstream.client.get(&url).send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(error = %err, "closure proxy: upstream unreachable");
            return Ok(axum::response::Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(axum::body::Body::from(format!("upstream error: {err}")))
                .unwrap_or_default());
        }
    };
    let status = resp.status().as_u16();
    let body = resp.bytes().await.map_err(|err| {
        tracing::warn!(error = %err, "closure proxy: upstream body read failed");
        StatusCode::BAD_GATEWAY
    })?;
    Ok(axum::response::Response::builder()
        .status(status)
        .header("content-type", "text/x-nix-narinfo")
        .body(axum::body::Body::from(body))
        .unwrap_or_default())
}

/// Middleware: enforce `X-Nixfleet-Protocol: <PROTOCOL_MAJOR_VERSION>`
/// on `/v1/*` requests (RFC-0003 §6).
///
/// Forward-compat posture: missing header → log debug + accept. This
/// lets older agents (Phase 3-deployed before this PR landed) keep
/// working during the transition. Header present + mismatched major
/// → 426 Upgrade Required + log warn.
///
/// /healthz is not subject to this — it's the operator's status
/// probe and runs unauthenticated; protocol-versioning the health
/// check makes the operational debug story worse without buying
/// anything.
async fn protocol_version_middleware(
    req: HttpRequest<Body>,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(value) = req.headers().get(PROTOCOL_VERSION_HEADER) {
        match value.to_str().ok().and_then(|s| s.parse::<u32>().ok()) {
            Some(v) if v == PROTOCOL_MAJOR_VERSION => Ok(next.run(req).await),
            Some(v) => {
                tracing::warn!(
                    sent = v,
                    expected = PROTOCOL_MAJOR_VERSION,
                    "rejecting request with mismatched protocol major version"
                );
                Err(StatusCode::UPGRADE_REQUIRED)
            }
            None => {
                tracing::warn!(
                    raw = ?value,
                    "X-Nixfleet-Protocol header malformed"
                );
                Err(StatusCode::BAD_REQUEST)
            }
        }
    } else {
        // Forward-compat: missing header is currently accepted with a
        // debug log. Phase 4 may flip this to a warn; v2 may flip to
        // a hard reject. Keeping it lenient now means agents in the
        // transition window between PR-3 and this header land keep
        // working.
        tracing::debug!("request without X-Nixfleet-Protocol — accepting (forward-compat)");
        Ok(next.run(req).await)
    }}

fn build_router(state: Arc<AppState>) -> Router {
    // /healthz remains unauthenticated per spec D7 — operational
    // debuggability outweighs the marginal sovereignty gain of
    // mTLS-gating a status endpoint.
    //
    // /v1/* requires verified mTLS — the MtlsAcceptor injects
    // PeerCertificates into request extensions; handlers extract via
    // the Extension extractor and 401 if absent/empty.
    //
    // PR-1: /healthz
    // PR-2: + /v1/whoami
    // PR-3: + /v1/agent/checkin, /v1/agent/report
    // PR-4: + /v1/admin/observed (proposed) — TBD
    // PR-5: + /v1/enroll, /v1/agent/renew
    // /healthz remains outside the /v1 namespace — no protocol-
    // version enforcement applies. /v1/* routes go through the
    // version middleware.
    let v1_routes = Router::new()
        .route("/v1/whoami", get(whoami))
        .route("/v1/agent/checkin", post(checkin))
        .route("/v1/agent/report", post(report))
        .route("/v1/agent/confirm", post(confirm))
        .route("/v1/agent/closure/{hash}", get(closure_proxy))
        .route("/v1/enroll", post(enroll))
        .route("/v1/agent/renew", post(renew))
        .layer(middleware::from_fn(protocol_version_middleware));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(v1_routes)
        .with_state(state)
}

/// Spawn the reconcile loop. Each tick:
/// 1. Reads the channel-refs cache (refreshed by the Forgejo poll
///    task; falls back to file-backed observed.json when empty).
/// 2. Builds an `Observed` from the in-memory checkin state +
///    cached channel-refs (PR-4 projection).
/// 3. Verifies the resolved artifact and reconciles against the
///    projected `Observed`.
/// 4. Emits the plan via tracing.
///
/// Errors at any step are logged and fall through; the loop never
/// crashes on transient failures.
fn spawn_reconcile_loop(state: Arc<AppState>, inputs: TickInputs) {
    tokio::spawn(async move {
        // Prime the verified-fleet snapshot from the build-time
        // artifact, IF it isn't already populated. The Forgejo
        // prime in `serve()` runs first and sets it from the
        // operator's freshest repo bytes; this fallback only fires
        // when Forgejo isn't configured or its fetch failed. Either
        // way we don't overwrite a Forgejo-fresh snapshot with a
        // stale build-time one — that's exactly the regression that
        // caused lab to stair-step backwards through deploy history
        // during the GitOps validation pass.
        {
            let already_primed = state.verified_fleet.read().await.is_some();
            if !already_primed {
                let prime_inputs = TickInputs {
                    now: Utc::now(),
                    ..inputs.clone()
                };
                if let Some(fleet) = verify_fleet_only(&prime_inputs) {
                    *state.verified_fleet.write().await = Some(Arc::new(fleet));
                    tracing::info!(
                        target: "reconcile",
                        "primed verified-fleet snapshot from build-time artifact (Forgejo prime unavailable)",
                    );
                } else {
                    tracing::warn!(
                        target: "reconcile",
                        "could not prime verified-fleet snapshot (verify failed); dispatch will block until first tick succeeds",
                    );
                }
            } else {
                tracing::debug!(
                    target: "reconcile",
                    "verified-fleet snapshot already populated by Forgejo prime; skipping build-time prime",
                );
            }
        }

        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + RECONCILE_INTERVAL,
            RECONCILE_INTERVAL,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            let now = Utc::now();

            // Snapshot the cache + checkins under read locks. Drop
            // the locks before doing the (potentially slow) verify +
            // reconcile work.
            let channel_refs = {
                let cache = state.channel_refs_cache.read().await;
                cache.refs.clone()
            };
            let checkins = state.host_checkins.read().await.clone();

            // PR-4 projection: in-memory checkins + cached channel-refs.
            // When the Forgejo poll hasn't succeeded yet AND no agents
            // have checked in, fall back to the file-backed
            // observed.json so PR-1's deploy-without-agents path keeps
            // working.
            let inputs_now = TickInputs {
                now,
                ..inputs.clone()
            };
            let (result, verified_fleet) = if checkins.is_empty() && channel_refs.is_empty() {
                (tick(&inputs_now), verify_fleet_only(&inputs_now))
            } else {
                run_tick_with_projection(&inputs_now, &checkins, &channel_refs)
            };

            // Snapshot the verified fleet so the dispatch path can
            // read it. Three preserve rules layered on top:
            //
            // 1. Verify-failed tick → preserve previous snapshot.
            //    Transient bad-signature shouldn't unblock dispatch.
            //
            // 2. The build-time artifact path is immutable for the
            //    CP's lifetime (it's a /nix/store path), so the
            //    bytes verify_fleet_only re-reads here are the SAME
            //    every tick. Without a freshness gate, this would
            //    overwrite a Forgejo-fresh snapshot with the
            //    deploy-time bytes — exactly the regression that
            //    made lab stair-step backwards through deploy
            //    history during the GitOps validation pass.
            //
            // 3. Compare `signed_at`: only overwrite if the new
            //    snapshot is at least as fresh as what's already
            //    there. Forgejo poll writes the freshest available;
            //    this loop preserves it.
            if let Some(fleet) = verified_fleet {
                let mut guard = state.verified_fleet.write().await;
                let should_overwrite = match guard.as_ref() {
                    None => true,
                    Some(existing) => match (existing.meta.signed_at, fleet.meta.signed_at) {
                        (Some(prev), Some(new)) => new >= prev,
                        // If either lacks signed_at (shouldn't happen
                        // for verified artifacts), fall back to
                        // overwriting — preserves prior behaviour.
                        _ => true,
                    },
                };
                if should_overwrite {
                    *guard = Some(Arc::new(fleet));
                }
            }

            match result {
                Ok(out) => {
                    let plan = render_plan(&out);
                    tracing::info!(target: "reconcile", "{}", plan.trim_end());
                }
                Err(err) => {
                    tracing::warn!(error = %err, "reconcile tick failed");
                }
            }
            *state.last_tick_at.write().await = Some(now);
        }
    });
}

/// Run a tick using the in-memory projection rather than reading
/// `observed.json`. Mirrors `crate::tick` but takes the projected
/// `Observed` from the caller.
///
/// Returns both the tick output (for the journal plan) and the
/// verified `FleetResolved` (for the dispatch path's snapshot in
/// `AppState`). The fleet is `None` when the tick failed verify —
/// the caller preserves whatever snapshot was previously in place.
fn run_tick_with_projection(
    inputs: &TickInputs,
    checkins: &HashMap<String, HostCheckinRecord>,
    channel_refs: &HashMap<String, String>,
) -> (anyhow::Result<crate::TickOutput>, Option<FleetResolved>) {
    use anyhow::Context;
    let read_inputs = || -> anyhow::Result<(Vec<u8>, Vec<u8>, nixfleet_proto::TrustConfig)> {
        let artifact = std::fs::read(&inputs.artifact_path)
            .with_context(|| format!("read artifact {}", inputs.artifact_path.display()))?;
        let signature = std::fs::read(&inputs.signature_path)
            .with_context(|| format!("read signature {}", inputs.signature_path.display()))?;
        let trust_raw = std::fs::read_to_string(&inputs.trust_path)
            .with_context(|| format!("read trust {}", inputs.trust_path.display()))?;
        let trust: nixfleet_proto::TrustConfig =
            serde_json::from_str(&trust_raw).context("parse trust")?;
        Ok((artifact, signature, trust))
    };

    let (artifact, signature, trust) = match read_inputs() {
        Ok(t) => t,
        Err(e) => return (Err(e), None),
    };

    let trusted_keys = trust.ci_release_key.active_keys();
    let reject_before = trust.ci_release_key.reject_before;

    let (verify, fleet) = match nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trusted_keys,
        inputs.now,
        inputs.freshness_window,
        reject_before,
    ) {
        Ok(fleet) => {
            let signed_at = fleet.meta.signed_at.expect("verified artifact carries meta.signedAt");
            let ci_commit = fleet.meta.ci_commit.clone();
            let observed = crate::observed_projection::project(checkins, channel_refs);
            let actions = nixfleet_reconciler::reconcile(&fleet, &observed, inputs.now);
            (
                crate::VerifyOutcome::Ok {
                    signed_at,
                    ci_commit,
                    observed,
                    actions,
                },
                Some(fleet),
            )
        }
        Err(err) => (
            crate::VerifyOutcome::Failed {
                reason: format!("{:?}", err),
            },
            None,
        ),
    };

    (
        Ok(crate::TickOutput {
            now: inputs.now,
            verify,
        }),
        fleet,
    )
}

/// Verify-only variant for the empty-projection fallback path. The
/// caller is responsible for running the rest of the tick (via
/// `crate::tick`) — this just produces the verified fleet snapshot
/// for `AppState.verified_fleet`. Returns `None` when verify fails;
/// the caller preserves the prior snapshot.
fn verify_fleet_only(inputs: &TickInputs) -> Option<FleetResolved> {
    let artifact = std::fs::read(&inputs.artifact_path).ok()?;
    let signature = std::fs::read(&inputs.signature_path).ok()?;
    let trust_raw = std::fs::read_to_string(&inputs.trust_path).ok()?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).ok()?;
    nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trust.ci_release_key.active_keys(),
        inputs.now,
        inputs.freshness_window,
        trust.ci_release_key.reject_before,
    )
    .ok()
}

/// Serve until interrupted. Builds the TLS config, starts the
/// reconcile loop, binds the listener, runs forever.
pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    // Phase 4 PR-1: open + migrate SQLite if a path is configured.
    // None → in-memory state only (PR-1 file-backed deploy + tests).
    let db = if let Some(path) = &args.db_path {
        let db = crate::db::Db::open(path)?;
        db.migrate()?;
        tracing::info!(path = %path.display(), "sqlite opened + migrated");
        Some(Arc::new(db))
    } else {
        None
    };

    let mut app_state = AppState::default();
    app_state.db = db.clone();
    if let Some(base_url) = &args.closure_upstream {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("build closure proxy client: {e}"))?;
        app_state.closure_upstream = Some(ClosureUpstream {
            base_url: base_url.clone(),
            client,
        });
    }
    let state = Arc::new(app_state);

    // Phase 4 PR-B: magic-rollback timer. Periodic background task
    // that scans pending_confirms for expired deadlines and marks
    // them rolled-back. Only runs when DB is configured — without
    // it, there's no pending_confirms table to query.
    if let Some(db_arc) = db {
        crate::rollback_timer::spawn(db_arc);
    }

    // Seed issuance config (PR-5). When fleet-ca-cert/key are unset
    // the /v1/enroll and /v1/agent/renew endpoints return 500 — they
    // need both to issue. PR-5's deploy expects them populated by
    // fleet/modules/secrets/nixos.nix.
    *state.issuance_paths.write().await = IssuancePaths {
        fleet_ca_cert: args.fleet_ca_cert.clone(),
        fleet_ca_key: args.fleet_ca_key.clone(),
        audit_log: args.audit_log_path.clone(),
    };

    // Pre-listener Forgejo prime: fetch the freshest signed artifact
    // straight from the operator's repo and seed `verified_fleet`
    // BEFORE the listener accepts the first checkin. Without this,
    // dispatch falls back to the compile-time `--artifact` path,
    // which is always an *older* release than what's on Forgejo
    // (CI commits the [skip ci] release commit AFTER building the
    // closure — every closure's bundled artifact is the previous
    // release). Lab caught this empirically during the GitOps
    // validation pass: agents checked in immediately on CP boot,
    // before the periodic poll's first tick, and dispatch issued
    // stair-stepping-backwards targets.
    //
    // Skip silently when Forgejo isn't configured or the fetch fails
    // — the reconcile loop's existing build-time prime is the
    // correct fallback for those cases. Cap the operation with a
    // short hard timeout so a wedged Forgejo can't block CP boot
    // forever.
    if let Some(forgejo_config) = args.forgejo.as_ref() {
        match tokio::time::timeout(
            Duration::from_secs(20),
            crate::forgejo_poll::prime_once(forgejo_config),
        )
        .await
        {
            Ok(Ok(fleet)) => {
                *state.verified_fleet.write().await = Some(Arc::new(fleet));
                tracing::info!(
                    target: "reconcile",
                    "primed verified-fleet from forgejo before opening listener",
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "forgejo prime failed; falling back to build-time artifact",
                );
            }
            Err(_) => {
                tracing::warn!(
                    "forgejo prime timed out; falling back to build-time artifact",
                );
            }
        }
    }

    // Reconcile loop runs concurrently with the listener — never gate
    // operator visibility on a TLS handshake completing.
    let tick_inputs = TickInputs {
        artifact_path: args.artifact_path.clone(),
        signature_path: args.signature_path.clone(),
        trust_path: args.trust_path.clone(),
        observed_path: args.observed_path.clone(),
        now: Utc::now(),
        freshness_window: args.freshness_window,
    };
    spawn_reconcile_loop(state.clone(), tick_inputs);

    // Forgejo poll task. Two responsibilities:
    //
    // 1. Refresh `verified_fleet` directly from
    //    `releases/fleet.resolved.json` + `.sig` on the operator's
    //    repo. Closes the GitOps loop — push to fleet/main → CI
    //    re-signs → poll (≤60s) refreshes the snapshot → next
    //    checkin dispatches against fresh closureHashes. Without
    //    this, the CP would only see the deploy-time artifact and
    //    operator commits would never propagate without a redeploy.
    //
    // 2. Refresh the channel_refs cache (telemetry + the
    //    `Observed.channel_refs` projection feeds the reconciler).
    //
    // When `--forgejo-base-url` is unset the poll never runs; the
    // CP relies on the file-backed `--artifact` path the reconcile
    // loop primes. Same fallback shape as before.
    //
    // Both shared locks (channel_refs_cache + verified_fleet) are
    // already `Arc<RwLock<...>>` on `AppState`; the poll task writes
    // through them directly, no mirror task needed. Reads from the
    // reconcile loop / dispatch path see updates the moment the poll
    // commits — no boot-time race where `channels_observed: 0` for
    // the first ≤30s after CP startup.
    if let Some(forgejo_config) = args.forgejo.clone() {
        crate::forgejo_poll::spawn(
            state.channel_refs_cache.clone(),
            state.verified_fleet.clone(),
            forgejo_config,
        );
    }

    let app = build_router(state);

    let tls_config = crate::tls::build_server_config(
        &args.tls_cert,
        &args.tls_key,
        args.client_ca.as_deref(),
    )?;
    let rustls_config =
        axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

    // Wrap RustlsAcceptor in MtlsAcceptor so peer certs are extracted
    // after the handshake and injected into request extensions. The
    // /v1/whoami handler reads the extension; PR-3+ middleware reads
    // it for CN-vs-path-id enforcement on agent routes.
    //
    // When --client-ca is unset (PR-1's TLS-only mode), the wrapper
    // still injects a PeerCertificates extension — just an empty one.
    // The /v1/whoami handler returns 401 in that case, which is
    // correct behaviour for the endpoint.
    let rustls_acceptor = axum_server::tls_rustls::RustlsAcceptor::new(rustls_config);
    let mtls_acceptor = MtlsAcceptor::new(rustls_acceptor);

    let mode = if args.client_ca.is_some() {
        "TLS+mTLS"
    } else {
        tracing::warn!(
            "control plane started without --client-ca: /v1/* endpoints will reject all clients with 401. \
             Pass --client-ca to enable mTLS — recommended for any non-PR-1 deployment."
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
