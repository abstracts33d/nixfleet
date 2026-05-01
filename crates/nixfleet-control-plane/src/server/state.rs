//! Shared state + configuration types for the long-running server.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{CheckinRequest, ReportRequest};
use nixfleet_proto::FleetResolved;
use tokio::sync::RwLock;

pub(super) const REPORT_RING_CAP: usize = 32;

pub(super) const NEXT_CHECKIN_SECS: u32 = 60;

pub(super) const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Must remain ≥ agent poll-budget + slack: a deadline shorter than
/// the agent's fire-and-forget poll window triggers magic-rollback
/// while the agent is still polling, cascading into a 410 + local
/// rollback chain. 360s = 300s poll budget + 60s slack for
/// clock skew + closure download tail latency.
pub const DEFAULT_CONFIRM_DEADLINE_SECS: i64 = 360;

#[derive(Debug, Clone)]
pub struct ServeArgs {
    pub listen: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub client_ca: Option<PathBuf>,
    /// Used by issuance to chain new agent certs. Often the same
    /// path as `client_ca`.
    pub fleet_ca_cert: Option<PathBuf>,
    pub fleet_ca_key: Option<PathBuf>,
    pub audit_log_path: Option<PathBuf>,
    pub artifact_path: PathBuf,
    pub signature_path: PathBuf,
    pub trust_path: PathBuf,
    /// File-backed observed-state fallback. The in-memory projection
    /// from check-ins is preferred; this is used only when no agents
    /// have checked in AND `channel_refs` is None.
    pub observed_path: PathBuf,
    pub freshness_window: Duration,
    pub confirm_deadline_secs: i64,
    /// GitOps fleet snapshot. None → CP relies on the file-backed
    /// `--artifact` path alone. Source-agnostic (Forgejo raw, GitHub
    /// raw, GitLab raw, plain HTTP).
    pub channel_refs: Option<crate::channel_refs_poll::ChannelRefsSource>,
    /// GitOps revocations sidecar. None → operators continue with
    /// direct DB writes (legacy path).
    pub revocations: Option<crate::revocations_poll::RevocationsSource>,
    /// None → in-memory state only.
    pub db_path: Option<PathBuf>,
    /// Base URL of a nix binary cache the CP proxies
    /// `/v1/agent/closure/<hash>` to. None → endpoint returns 501.
    pub closure_upstream: Option<String>,
    /// Directory containing pre-signed `<rolloutId>.{json,sig}` pairs
    /// produced by nixfleet-release. None → manifest distribution
    /// falls back to `rollouts_source` HTTP fetch; if both are None,
    /// `GET /v1/rollouts/<id>` returns 503.
    pub rollouts_dir: Option<PathBuf>,
    /// HTTP-fetched rollout manifests source. None → fall back to
    /// `rollouts_dir`. Useful for fleets where the manifests can't
    /// live in `inputs.self` at closure-build time (the typical case
    /// since `nixfleet-release` writes them after building closures).
    pub rollouts_source: Option<crate::rollouts_source::RolloutsSource>,
}

#[derive(Debug, Clone)]
pub struct HostCheckinRecord {
    pub last_checkin: DateTime<Utc>,
    pub checkin: CheckinRequest,
}

/// `signature_status` is the evidence-verify verdict for events
/// that carry a host-key-bound signature. The full set is enumerated
/// in `report_handler::compute_signature_status`. None for variants
/// that intentionally don't sign (pre-fire announcements, enrollment
/// failures, trust-root parse errors, the `Other` catch-all) or for
/// events posted before the field was added.
#[derive(Debug, Clone)]
pub struct ReportRecord {
    pub event_id: String,
    pub received_at: DateTime<Utc>,
    pub report: ReportRequest,
    pub signature_status: Option<nixfleet_reconciler::evidence::SignatureStatus>,
}

#[derive(Clone, Debug)]
pub struct ClosureUpstream {
    pub base_url: String,
    pub client: reqwest::Client,
}

#[derive(Debug, Clone, Default)]
pub struct IssuancePaths {
    pub fleet_ca_cert: Option<PathBuf>,
    pub fleet_ca_key: Option<PathBuf>,
    pub audit_log: Option<PathBuf>,
}

/// `db` is Optional so file-backed deploys + tests can run without
/// SQLite. `verified_fleet` and `channel_refs_cache` are
/// `Arc<RwLock<_>>` so the poll task writes through them directly
/// without a mirror task; the reconcile loop's freshness gate
/// preserves the upstream-fresh snapshot.
pub struct AppState {
    pub last_tick_at: RwLock<Option<DateTime<Utc>>>,
    pub host_checkins: RwLock<HashMap<String, HostCheckinRecord>>,
    pub host_reports: RwLock<HashMap<String, VecDeque<ReportRecord>>>,
    pub channel_refs_cache: Arc<RwLock<crate::channel_refs_poll::ChannelRefsCache>>,
    pub issuance_paths: RwLock<IssuancePaths>,
    pub db: Option<Arc<crate::db::Db>>,
    pub closure_upstream: Option<ClosureUpstream>,
    pub verified_fleet: Arc<RwLock<Option<Arc<FleetResolved>>>>,
    /// SHA-256 hex of the canonical bytes of `verified_fleet`. Updated
    /// in lockstep with verified_fleet by the channel-refs poll. Used
    /// by `compute_rollout_id_for_channel` to anchor every rolloutId
    /// advertisement to the specific signed snapshot it was projected
    /// from (RFC-0002 §4.4 anchor).
    pub fleet_resolved_hash: Arc<RwLock<Option<String>>>,
    pub confirm_deadline_secs: i64,
    /// Filesystem path to the directory holding pre-signed rollout
    /// manifests. `GET /v1/rollouts/<rolloutId>` looks here first.
    pub rollouts_dir: Option<PathBuf>,
    /// HTTP-fetched rollout manifests source. Used when `rollouts_dir`
    /// is None or the requested manifest is missing on disk.
    pub rollouts_source: Option<crate::rollouts_source::RolloutsSource>,
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
            issuance_paths: RwLock::new(IssuancePaths::default()),
            db: None,
            closure_upstream: None,
            verified_fleet: Arc::new(RwLock::new(None)),
            fleet_resolved_hash: Arc::new(RwLock::new(None)),
            confirm_deadline_secs: DEFAULT_CONFIRM_DEADLINE_SECS,
            rollouts_dir: None,
            rollouts_source: None,
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
