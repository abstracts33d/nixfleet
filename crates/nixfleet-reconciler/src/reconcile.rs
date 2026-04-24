//! Top-level `reconcile` function. Implementation follows in Phase D.

use crate::{Action, Observed};
use chrono::{DateTime, Utc};
use nixfleet_proto::FleetResolved;

pub fn reconcile(_fleet: &FleetResolved, _observed: &Observed, _now: DateTime<Utc>) -> Vec<Action> {
    Vec::new()
}
