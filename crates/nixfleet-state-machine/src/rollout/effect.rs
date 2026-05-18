//! Rollout-level effects (RFC-0008 §5). Descriptive data; the CP applier
//! interprets each variant against the `rollouts` / `quarantined_closures`
//! derived-view tables (RFC-0008 §6.3 + §6.4).
//!
//! Effects-as-data discipline (RFC-0006 §3): the reducer cannot perform
//! I/O; the applier has one match arm per variant. Adding a variant
//! is a compiler-enforced change at every applier.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::rollout::state::{ChannelId, ClosureHash, RolloutId, RolloutState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutEffect {
    /// Persist a rollout-level state transition. Drives the
    /// `rollouts.state` column update; the applier co-writes the
    /// corresponding `event_log` row (`kind = 'rollout_event'`).
    RecordRolloutTransition {
        rollout_id: RolloutId,
        from: RolloutState,
        to: RolloutState,
        at: DateTime<Utc>,
    },
    /// Monotonic wave-index advance on the `rollouts.current_wave` column.
    UpdateCurrentWave { rollout_id: RolloutId, wave: u32 },
    /// A rollback completed; insert a row into `quarantined_closures`
    /// referencing the triggering `event_log` seq (NULL-able under v0.2.1
    /// baseline; RFC-0008 §6.1 item 3).
    InsertQuarantineFromRollout {
        channel: ChannelId,
        closure_hash: ClosureHash,
    },
    /// A rollout entered a terminal-set state (Terminal | Superseded |
    /// Failed | Reverted); schedule its retention-expiry event for the
    /// configured delay. The applier queues a delayed `RetentionExpired`
    /// re-entry into the reducer.
    SchedulePruning {
        rollout_id: RolloutId,
        delay_seconds: i64,
    },
}

impl RolloutEffect {
    pub fn kind(&self) -> &'static str {
        match self {
            RolloutEffect::RecordRolloutTransition { .. } => "RecordRolloutTransition",
            RolloutEffect::UpdateCurrentWave { .. } => "UpdateCurrentWave",
            RolloutEffect::InsertQuarantineFromRollout { .. } => "InsertQuarantineFromRollout",
            RolloutEffect::SchedulePruning { .. } => "SchedulePruning",
        }
    }
}
