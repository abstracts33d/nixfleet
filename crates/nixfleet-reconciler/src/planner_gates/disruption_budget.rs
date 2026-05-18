//! Disruption-budget gate (new-shape). `max_in_flight` enforced at
//! dispatch time, summed across all active rollouts whose budgets share
//! a selector (matches the old gate's "max one workstation in flight,
//! ever" cross-rollout semantics).
//!
//! LOADBEARING: "in-flight" = `{Activating, Soaking}` (RFC-0002 §3).
//! `Pending` is explicitly NOT in-flight — a Pending host has not
//! received its Dispatch yet; counting it as in-flight causes a
//! self-block where a freshly-`OpenRollout`'d host saturates the
//! budget against its own Pending status and never transitions to
//! Activating. `Failed`/`Reverted`/`Converged` are terminal and also
//! not in-flight.
//!
//! ## Within-tick accumulator
//!
//! Excluding Pending from `is_in_flight` exposes a within-tick
//! over-commit risk: a single `plan_next()` tick can emit N
//! `QueueDispatch`es for N Pending hosts, each seeing `in_flight = 0`
//! at gate-check time because none have transitioned to Activating
//! yet. With `max_in_flight = 1` that's an N-fold over-commit.
//!
//! The planner threads `tick_dispatched: &HashMap<Selector, u32>`
//! through `evaluate_for_dispatch` and increments the per-budget
//! count after each `QueueDispatch`. The gate consults
//! `in_flight + tick_count` against `max`.

use std::collections::HashMap;

use nixfleet_proto::Selector;
use nixfleet_state_machine::HostState;

use crate::planner_gates::GateBlock;
use crate::planner_types::{FleetState, HostId, RolloutId};

/// Key used by `plan_next`'s within-tick accumulator. The same
/// `Selector` value that identifies a budget across rollouts also
/// keys the per-tick counter — one budget identity, one place.
pub type BudgetId = Selector;

pub fn check(
    fleet_state: &FleetState,
    rollout_id: &RolloutId,
    host: &HostId,
    tick_dispatched: &HashMap<BudgetId, u32>,
) -> Option<GateBlock> {
    let rollout = fleet_state.rollouts.get(rollout_id)?;

    for budget in &rollout.budgets {
        if !budget.hosts.iter().any(|h| h == host) {
            continue;
        }
        let max = match budget.max_in_flight {
            Some(m) => m,
            None => continue,
        };
        let in_flight = in_flight_count(fleet_state, &budget.selector);
        let pending_this_tick = tick_dispatched.get(&budget.selector).copied().unwrap_or(0);
        let total = in_flight.saturating_add(pending_this_tick);
        if total >= max {
            return Some(GateBlock::DisruptionBudget {
                in_flight: total,
                max,
                selector_summary: budget.selector.summary(),
            });
        }
    }
    None
}

fn in_flight_count(fleet_state: &FleetState, selector: &Selector) -> u32 {
    let mut count: u32 = 0;
    for (rollout_id, summary) in &fleet_state.rollouts {
        let Some(matching_budget) = summary.budgets.iter().find(|b| &b.selector == selector) else {
            continue;
        };
        for ((rid, hostname), state) in &fleet_state.host_states {
            if rid != rollout_id {
                continue;
            }
            if !matching_budget.hosts.iter().any(|h| h == hostname) {
                continue;
            }
            if is_in_flight(state.state) {
                count += 1;
            }
        }
    }
    count
}

fn is_in_flight(state: HostState) -> bool {
    // LOADBEARING: `Pending` is NOT in-flight. A Pending host has not
    // acked a Dispatch yet (and may not have been issued one in this
    // tick). Counting it as in-flight self-blocks the host from
    // transitioning to Activating.
    matches!(state, HostState::Activating | HostState::Soaking)
}
