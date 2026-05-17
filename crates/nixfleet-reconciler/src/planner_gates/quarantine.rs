//! Anti-thrash quarantine gate (new-shape). Same predicate as
//! `gates::quarantine`, just parameterized directly on
//! `(channel, target_closure, quarantines)` instead of digging through
//! `Observed`. Phase 6g deletes the old version.
//!
//! No FleetState dependency — quarantines are a closed input.

use crate::planner_gates::GateBlock;
use crate::planner_types::{ChannelId, ClosureHash, QuarantineSet};

pub fn check(
    quarantines: &QuarantineSet,
    channel: &ChannelId,
    target_closure: &ClosureHash,
) -> Option<GateBlock> {
    let set = quarantines.get(channel)?;
    if set.contains(target_closure) {
        Some(GateBlock::Quarantined {
            channel: channel.clone(),
            closure_hash: target_closure.clone(),
        })
    } else {
        None
    }
}
