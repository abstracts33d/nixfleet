//! ChannelEdges gate — predecessor channel must converge before successor opens.
//!
//! Migrated from `crate::reconcile::predecessor_channel_blocking`. The
//! reconciler's `reconcile()` main loop still uses
//! `check_for_channel` directly (channel-level, not host-level — it
//! decides whether to emit `OpenRollout` for a channel whose ref
//! changed). The dispatch endpoint uses `check` via
//! `gates::evaluate_for_host`.
//!
//! Both call sites end up in the same predicate (`channel_blocked`),
//! so adding a new edge case touches one function and is enforced
//! everywhere.

use crate::host_state::HostRolloutState;
use crate::observed::{Observed, Rollout};
use nixfleet_proto::FleetResolved;
use std::collections::HashSet;

use super::{GateBlock, GateInput};

/// Per-host gate entry. Derives the host's channel from `fleet.hosts`
/// and dispatches to `check_for_channel`.
pub fn check(input: &GateInput) -> Option<GateBlock> {
    let host_channel = input
        .fleet
        .hosts
        .get(input.host)
        .map(|h| h.channel.as_str())?;
    check_for_channel(
        input.fleet,
        input.observed,
        input.emitted_opens_in_tick,
        host_channel,
        input.conservative_on_missing_state,
    )
    .map(|predecessor| GateBlock::ChannelEdges {
        predecessor_channel: predecessor,
    })
}

/// Channel-level entry. Returns the predecessor channel name when
/// `channel` is held, else `None`.
///
/// Public so the reconciler's `reconcile()` main loop and the
/// dashboard's live `/v1/deferrals` route can consult the same
/// predicate.
pub fn check_for_channel(
    fleet: &FleetResolved,
    observed: &Observed,
    emitted_opens_in_tick: &HashSet<String>,
    channel: &str,
    conservative_on_missing_state: bool,
) -> Option<String> {
    fleet
        .channel_edges
        .iter()
        .filter(|e| e.after == channel)
        .find_map(|e| {
            channel_blocked(
                fleet,
                observed,
                emitted_opens_in_tick,
                &e.before,
                conservative_on_missing_state,
            )
            .then(|| e.before.clone())
        })
}

/// Single-predecessor check. The shared predicate behind every entry
/// point — `check`, `check_for_channel`, and the dashboard live read all
/// route here.
///
/// Source-of-truth precedence:
///   1. If a rollout for `predecessor` exists in `observed.active_rollouts`,
///      ITS state wins. A converged rollout (every host Soaked or
///      Converged) means the predecessor is done.
///   2. Otherwise, if the predecessor was emitted in this reconcile
///      tick, it counts as active.
///   3. Otherwise, in conservative mode (dispatch endpoint, fresh-boot
///      protection), block if the fleet declares hosts on the
///      predecessor channel. The reconciler's non-conservative mode
///      lets it through — `emitted_opens_in_tick` is the authoritative
///      in-tick signal there.
fn channel_blocked(
    fleet: &FleetResolved,
    observed: &Observed,
    emitted_opens_in_tick: &HashSet<String>,
    predecessor: &str,
    conservative_on_missing_state: bool,
) -> bool {
    let db_rollout = observed
        .active_rollouts
        .iter()
        .find(|r| r.channel == predecessor);
    match db_rollout {
        Some(r) => rollout_is_active_for_ordering(r),
        None => {
            if emitted_opens_in_tick.contains(predecessor) {
                return true;
            }
            if conservative_on_missing_state {
                fleet.hosts.values().any(|h| h.channel == predecessor)
            } else {
                false
            }
        }
    }
}

/// True if the rollout still has work outstanding from the perspective
/// of `channelEdges` ordering.
///
/// `Soaked` and `Converged` count as terminal-for-ordering: the host
/// has cleared its soak window. Treating only `Converged` as terminal
/// would block the successor channel during the gap between SoakHost
/// transitions and the next reconcile tick's `ConvergeRollout` action —
/// small in practice but adds latency and is semantically wrong.
///
/// Empty `host_states` (a freshly-recorded rollout that has not yet
/// dispatched any host) is also treated as active — the rollout has
/// work to do, just hasn't started.
///
/// `Failed` / `Reverted` are NOT terminal-for-ordering: the predecessor
/// is in trouble and the successor must wait.
fn rollout_is_active_for_ordering(r: &Rollout) -> bool {
    if r.host_states.is_empty() {
        return true;
    }
    !r.host_states
        .values()
        .all(|s| matches!(s, HostRolloutState::Soaked | HostRolloutState::Converged))
}
