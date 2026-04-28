//! Shared state + configuration types for the long-running server.
//!
//! Pulled out of the monolithic `server.rs` so the handler /
//! middleware / reconcile-loop modules can each take a thin
//! dependency on `AppState` without dragging the whole serve()
//! surface along. Public re-export from `server::mod` keeps the
//! crate's external API unchanged.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{CheckinRequest, ReportRequest};
use nixfleet_proto::FleetResolved;
use tokio::sync::RwLock;

/// Per-host event ring buffer cap. `/v1/agent/report` is in-memory
/// only; SQLite-backed persistence for `host_reports` is still
/// pending. 32 entries is enough to spot a flapping host without
/// unbounded memory growth.
pub(super) const REPORT_RING_CAP: usize = 32;

/// Returned to the agent in `CheckinResponse.next_checkin_secs`.
/// Default 60s. The dispatch loop doesn't currently shape this
/// per-host; future load-shaping (RFC §5) hashes hostname into a
/// poll slot.
pub(super) const NEXT_CHECKIN_SECS: u32 = 60;

/// Reconcile loop cadence — D2 default. Operator-visible drift (host
/// failed to check in) shows up in the journal within one cycle;
/// slow enough not to spam.
pub(super) const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Time the dispatch loop gives an agent to fetch + activate +
/// confirm a target before the magic-rollback timer marks the
/// pending row as `rolled-back`. 120s is the spec-D1 default —
/// enough headroom for a closure download + activation, short enough
/// that a stuck agent surfaces in the journal within one rollback-
/// timer tick.
pub(super) const CONFIRM_DEADLINE_SECS: i64 = 120;

/// Inputs the `serve` subcommand receives from clap.
#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub listen: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub client_ca: Option<PathBuf>,
    /// Fleet CA cert — used by issuance to chain new agent certs.
    /// Often the same path as `client_ca`.
    pub fleet_ca_cert: Option<PathBuf>,
    /// Fleet CA private key — issuance signs new agent certs with
    /// this. **Online on the CP per the deferred-trust-hardening
    /// design (issue #41).**
    pub fleet_ca_key: Option<PathBuf>,
    /// Path to the audit-log JSON-lines file.
    pub audit_log_path: Option<PathBuf>,
    pub artifact_path: PathBuf,
    pub signature_path: PathBuf,
    pub trust_path: PathBuf,
    /// File-backed observed-state fallback path. The in-memory
    /// projection from check-ins is preferred; this path is used only
    /// when no agents have checked in yet AND `channel_refs` is None
    /// (offline dev/test mode).
    pub observed_path: PathBuf,
    pub freshness_window: Duration,
    /// GitOps closure: when set, the channel-refs poll fetches the
    /// signed `fleet.resolved.json` + `.sig` from the configured
    /// upstream URLs every 60s, verifies, and refreshes
    /// `verified_fleet`. When `None`, the CP relies on the
    /// file-backed `--artifact` path alone. The source is
    /// implementation-agnostic — Forgejo raw URLs, GitHub
    /// `raw.githubusercontent.com`, GitLab `/-/raw/`, plain HTTP, etc.
    pub channel_refs: Option<crate::channel_refs_poll::ChannelRefsSource>,
    /// GitOps revocations: when set, the revocations poll fetches
    /// the signed `revocations.json` + `.sig` from the configured
    /// upstream URLs every 60s, verifies against the same
    /// ciReleaseKey trust roots as channel-refs, and replays
    /// entries into `cert_revocations` (gap C). When `None`, the
    /// CP runs without a signed revocations source — operators
    /// who haven't migrated to the signed-sidecar workflow
    /// continue to use direct DB writes (legacy path).
    pub revocations: Option<crate::revocations_poll::RevocationsSource>,
    /// SQLite path. When `Some`, the DB is opened + migrated at
    /// startup. When `None`, in-memory state only.
    pub db_path: Option<PathBuf>,
    /// Closure proxy upstream. Base URL of a nix binary cache
    /// (harmonia, attic, cachix, etc.) the CP proxies
    /// `/v1/agent/closure/<hash>` requests to. `None` → endpoint
    /// returns 501.
    pub closure_upstream: Option<String>,
}

/// Most-recent checkin per host. The projection feeds this into the
/// reconciler.
#[derive(Debug, Clone)]
pub struct HostCheckinRecord {
    pub last_checkin: DateTime<Utc>,
    pub checkin: CheckinRequest,
}

/// In-memory record of an event report. Bounded ring buffer per
/// host (cap = `REPORT_RING_CAP`). DB-backed persistence is deferred.
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

/// Issuance paths. Stored on `AppState` so handlers can read them
/// at request time.
#[derive(Debug, Clone, Default)]
pub struct IssuancePaths {
    pub fleet_ca_cert: Option<PathBuf>,
    pub fleet_ca_key: Option<PathBuf>,
    pub audit_log: Option<PathBuf>,
}

/// Server-wide shared state.
///
/// `db` is `Option<Arc<Db>>` so file-backed deploy + tests run
/// without standing up SQLite. Production deploys wire it via
/// `--db-path`.
///
/// `verified_fleet` and `channel_refs_cache` are both `Arc<RwLock<>>`
/// so the channel-refs poll task can write through them directly
/// without a mirror task. The reconcile loop's per-tick build-time
/// verify uses a `signed_at` freshness gate before overwriting, so
/// the upstream-fresh snapshot is preserved.
pub struct AppState {
    pub last_tick_at: RwLock<Option<DateTime<Utc>>>,
    pub host_checkins: RwLock<HashMap<String, HostCheckinRecord>>,
    pub host_reports: RwLock<HashMap<String, VecDeque<ReportRecord>>>,
    pub channel_refs_cache: Arc<RwLock<crate::channel_refs_poll::ChannelRefsCache>>,
    pub seen_token_nonces: RwLock<HashSet<String>>,
    pub issuance_paths: RwLock<IssuancePaths>,
    pub db: Option<Arc<crate::db::Db>>,
    pub closure_upstream: Option<ClosureUpstream>,
    pub verified_fleet: Arc<RwLock<Option<Arc<FleetResolved>>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_tick_at: RwLock::new(None),
            host_checkins: RwLock::new(HashMap::new()),
            host_reports: RwLock::new(HashMap::new()),
            channel_refs_cache: Arc::new(RwLock::new(
                crate::channel_refs_poll::ChannelRefsCache::default(),
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
