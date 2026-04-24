//! Edge predecessor ordering check (RFC-0002 §4.1).

use crate::observed::Rollout;
use nixfleet_proto::FleetResolved;

/// If `host`'s in-wave predecessors are NOT all Soaked/Converged, return
/// the name of the first incomplete predecessor. Otherwise `None`.
pub(crate) fn predecessor_blocking(
    fleet: &FleetResolved,
    rollout: &Rollout,
    host: &str,
) -> Option<String> {
    fleet
        .edges
        .iter()
        .filter(|e| e.before == host)
        .find_map(|e| {
            let s = rollout
                .host_states
                .get(&e.after)
                .map(String::as_str)
                .unwrap_or("Queued");
            if matches!(s, "Soaked" | "Converged") {
                None
            } else {
                Some(e.after.clone())
            }
        })
}
