//! Rollout-level event vocabulary (RFC-0012 §4). All CP-internal: the
//! applier synthesizes these from per-host events (RFC-0008 §4.2) and
//! channel-refs poll outcomes — agents never emit `RolloutEvent`s on the
//! wire.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::HostState;
use crate::rollout::state::{ChannelId, RolloutId};
use nixfleet_proto::ChannelRef;

pub type HostId = String;

/// Per-host state representation as seen by the rollout reducer.
///
/// The rollout reducer reasons about per-host transitions at a coarser
/// granularity than RFC-0008 §3's six states; this alias names the input
/// shape it accepts on `HostStateChanged`. Today it carries the full
/// RFC-0008 state (re-exported from the host reducer); future projection
/// to a coarser enum is a v0.3 optimisation.
pub type HostRolloutState = HostState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutEvent {
    /// Channel-refs poll detected a new ref; planner opened the rollout.
    /// First event in any rollout's lifecycle.
    RolloutOpened {
        rollout_id: RolloutId,
        channel: ChannelId,
        target_ref: ChannelRef,
        at: DateTime<Utc>,
    },
    /// A host has been dispatched into this rollout (RFC-0008 §4.1
    /// `Dispatch` queued). First `HostJoined` moves `Opening → Active`.
    HostJoined {
        rollout_id: RolloutId,
        host_id: HostId,
        wave: u32,
        at: DateTime<Utc>,
    },
    /// Per-host state transitioned (RFC-0008 §3). The rollout reducer
    /// aggregates these to drive the rollout-level state machine.
    HostStateChanged {
        rollout_id: RolloutId,
        host_id: HostId,
        from: HostRolloutState,
        to: HostRolloutState,
        at: DateTime<Utc>,
    },
    /// A wave's hosts all reached Soaked; planner is dispatching the next
    /// wave. Drives `Converging → Active`.
    WaveAdvanced {
        rollout_id: RolloutId,
        from_wave: u32,
        to_wave: u32,
        at: DateTime<Utc>,
    },
    /// Aggregate signal: all hosts in all waves Converged. Drives
    /// `Converging → Terminal`. Emitted by the reducer itself (not by an
    /// upstream worker) as a consequence of the last per-host `Converged`.
    RolloutTerminal {
        rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    /// Channel-refs poll detected a successor ref. Both rollouts are
    /// named in one event so the reducer can transition the predecessor
    /// (→ Superseded) and open the successor in lockstep.
    SuccessorOpened {
        superseded_rollout_id: RolloutId,
        successor_rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    /// Retention threshold elapsed for a `Terminal | Superseded | Failed |
    /// Reverted` rollout. Drives the terminal-set → `Pruned` transition.
    RetentionExpired {
        rollout_id: RolloutId,
        at: DateTime<Utc>,
    },
    /// Operator-issued state override (currently rare; CLI wiring is a
    /// future v0.2.x follow-up — RFC-0012 §13 + Phase 10 brief §9.3).
    OperatorClearance {
        rollout_id: RolloutId,
        operator: String,
        reason: String,
        at: DateTime<Utc>,
    },
}

impl RolloutEvent {
    /// Static event-kind name for logging / error reporting.
    pub fn kind(&self) -> &'static str {
        match self {
            RolloutEvent::RolloutOpened { .. } => "RolloutOpened",
            RolloutEvent::HostJoined { .. } => "HostJoined",
            RolloutEvent::HostStateChanged { .. } => "HostStateChanged",
            RolloutEvent::WaveAdvanced { .. } => "WaveAdvanced",
            RolloutEvent::RolloutTerminal { .. } => "RolloutTerminal",
            RolloutEvent::SuccessorOpened { .. } => "SuccessorOpened",
            RolloutEvent::RetentionExpired { .. } => "RetentionExpired",
            RolloutEvent::OperatorClearance { .. } => "OperatorClearance",
        }
    }
}
