//! Disruption budget evaluation (RFC-0002 §4.2).

use crate::observed::Observed;
use nixfleet_proto::FleetResolved;

/// Count hosts currently in-flight across all active rollouts.
pub(crate) fn in_flight_count(observed: &Observed, budget_hosts: &[String]) -> u32 {
    observed
        .active_rollouts
        .iter()
        .map(|r| {
            r.host_states
                .iter()
                .filter(|(h, st)| {
                    budget_hosts.iter().any(|b| b == *h)
                        && matches!(
                            st.as_str(),
                            "Dispatched" | "Activating" | "ConfirmWindow" | "Healthy"
                        )
                })
                .count() as u32
        })
        .sum()
}

/// For a given host, return the tightest (in_flight, max_in_flight) across
/// all budgets that include the host.
pub(crate) fn budget_max(
    fleet: &FleetResolved,
    observed: &Observed,
    host: &str,
) -> Option<(u32, u32)> {
    fleet
        .disruption_budgets
        .iter()
        .filter(|b| b.hosts.iter().any(|bh| bh == host))
        .filter_map(|b| {
            b.max_in_flight
                .map(|max| (in_flight_count(observed, &b.hosts), max))
        })
        .min_by_key(|(_, max)| *max)
}
