//! Rollout-level state (RFC-0012 §3). Eight states, all CP-internal.
//!
//! Parallel to the per-host `HostRolloutState` (RFC-0008 §3): same pure-
//! reducer shape, but operates on a `(channel, rollout_id)` aggregate
//! rather than a `(rollout_id, hostname)` per-host slot.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// `RolloutId` re-exported from `nixfleet-proto` (RFC-0012 §6.3);
// kept here for ergonomic `crate::rollout::state::RolloutId` access
// in the rollout reducer's callsites.
pub use nixfleet_proto::{ChannelRef, RolloutId};

pub type ChannelId = String;
pub type ClosureHash = String;

/// The eight rollout-level states per RFC-0012 §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RolloutState {
    /// Channel-refs poll detected new ref; rollout opened; no hosts
    /// dispatched yet.
    Opening,
    /// At least one host is in-flight
    /// (`Pending` / `Activating` / `Soaking` per RFC-0008).
    Active,
    /// All dispatched hosts reached `Soaked`; later waves remain to
    /// dispatch.
    Converging,
    /// All hosts in all waves are `Converged`; channel-edges may release.
    Terminal,
    /// Any host reached `Reverted` via the `rollback-and-halt` policy.
    Reverted,
    /// Any host stuck in `Failed` state without rollback
    /// (e.g., `halt-only` policy).
    Failed,
    /// A newer rollout for the same channel opened.
    Superseded,
    /// Retention timeout elapsed; in-memory state-machine instance is
    /// freed but the DB row persists (table remains re-derivable from
    /// `event_log`). Physical row deletion deferred to v0.3 retention-
    /// compaction (RFC-0012 §3 + §13).
    Pruned,
}

/// The full row shape the reducer operates on. Mirrors the on-disk
/// `rollouts` table (RFC-0012 §6.3) plus per-rollout in-memory aggregates
/// the reducer needs to decide transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutRecord {
    pub rollout_id: RolloutId,
    pub channel: ChannelId,
    pub target_ref: ChannelRef,
    pub state: RolloutState,
    pub current_wave: u32,
    /// `event_log.seq` of the `RolloutOpened` event that opened this
    /// rollout. NULL-able under the v0.2.1 baseline (RFC-0012 §6.1 item 3 +
    /// `.claude/plans/v0.2.1-followups.md` #1); tightens to NOT NULL when
    /// the event_log writer gains synchronous seq return.
    pub opened_event_log_seq: Option<i64>,
    /// `event_log.seq` of the most recent `rollout_event` that mutated
    /// this rollout. Same NULL-able caveat as `opened_event_log_seq`.
    pub last_transition_event_log_seq: Option<i64>,
    pub opened_at: DateTime<Utc>,
    pub terminal_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
}

impl RolloutState {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            RolloutState::Opening => "Opening",
            RolloutState::Active => "Active",
            RolloutState::Converging => "Converging",
            RolloutState::Terminal => "Terminal",
            RolloutState::Reverted => "Reverted",
            RolloutState::Failed => "Failed",
            RolloutState::Superseded => "Superseded",
            RolloutState::Pruned => "Pruned",
        }
    }

    /// Parse the SQLite stored string. Returns `None` on unknown values; the
    /// schema CHECK constraint should prevent that, but callers handle
    /// gracefully rather than crashing.
    pub fn from_db_str(s: &str) -> Option<Self> {
        Some(match s {
            "Opening" => RolloutState::Opening,
            "Active" => RolloutState::Active,
            "Converging" => RolloutState::Converging,
            "Terminal" => RolloutState::Terminal,
            "Reverted" => RolloutState::Reverted,
            "Failed" => RolloutState::Failed,
            "Superseded" => RolloutState::Superseded,
            "Pruned" => RolloutState::Pruned,
            _ => return None,
        })
    }

    /// Channel-edges treat `Terminal` and `Superseded` as equivalent
    /// release signals (RFC-0012 §3 invariant: "Superseded is terminal-for-
    /// ordering but not terminal-for-pruning").
    pub fn is_terminal_for_ordering(&self) -> bool {
        matches!(self, RolloutState::Terminal | RolloutState::Superseded)
    }
}
