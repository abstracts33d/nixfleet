//! Compliance-wave gate — earlier-wave hosts with outstanding compliance
//! failures hold dispatch of later-wave hosts under `enforce` mode.
//!
//! Migrated from `nixfleet_control_plane::wave_gate::evaluate_channel_gate`.
//! The migration uses the AGGREGATED form
//! (`Observed.compliance_failures_by_rollout`, populated from the DB-side
//! `outstanding_compliance_events_by_rollout` query that already excludes
//! `mismatch`/`malformed` signature statuses) — same data the
//! reconciler's `wave_blocked` event reads. Both layers go through this
//! gate at the dispatch decision; the reconciler's `Action::WaveBlocked`
//! is a separate concern (wave-promotion gate inside rollout_state.rs)
//! and stays untouched.
//!
//! Mode handling:
//!   - `disabled`: no-op.
//!   - `permissive`: counts outstanding events for observability but
//!     never blocks. Returns `None`.
//!   - `enforce`: blocks dispatch when any host in an EARLIER wave
//!     (strictly less than the requesting host's wave) has outstanding
//!     compliance failures recorded against the current rollout.
//!
//! Per-rollout grouping enforces resolution-by-replacement: events under
//! a superseded rollout never appear under the current rollout's key, so
//! a fresh deploy clears the gate without operator intervention.

use nixfleet_proto::compliance::GateMode;
use nixfleet_proto::Wave;

use crate::observed::Observed;

use super::{GateBlock, GateInput};

/// Outstanding compliance failures grouped per host, restricted to the
/// hosts in `wave_range` of `waves`. Returned vec is sorted+deduped.
///
/// LOADBEARING: same predicate consumed by both the dispatch gate
/// (waves 0..host_wave, exclusive — only EARLIER waves count) and the
/// reconciler's wave-promotion `Action::WaveBlocked` emission (waves
/// 0..=current_wave, inclusive — current wave's failures hold
/// promotion). Range is the only difference between call sites; one
/// helper means a fix to filtering / signature handling / per-host
/// grouping covers both.
pub fn outstanding_failures_in_waves(
    observed: &Observed,
    rollout_id: &str,
    waves: &[Wave],
    wave_range: std::ops::Range<usize>,
) -> Vec<(String, usize)> {
    let Some(per_host) = observed.compliance_failures_by_rollout.get(rollout_id) else {
        return Vec::new();
    };
    let mut out: Vec<(String, usize)> = Vec::new();
    for w in waves.iter().take(wave_range.end).skip(wave_range.start) {
        for h in &w.hosts {
            if let Some(&n) = per_host.get(h) {
                if n > 0 {
                    out.push((h.clone(), n));
                }
            }
        }
    }
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub fn check(input: &GateInput) -> Option<GateBlock> {
    let host_channel = input
        .fleet
        .hosts
        .get(input.host)
        .map(|h| h.channel.as_str())?;

    let channel = input.fleet.channels.get(host_channel)?;
    let mode = GateMode::from_wire_str(&channel.compliance.mode);
    if !mode.is_enforcing() {
        return None;
    }

    let host_wave = input.fleet.waves.get(host_channel).and_then(|waves| {
        waves
            .iter()
            .position(|w| w.hosts.iter().any(|h| h == input.host))
    });

    // Without a wave plan or with the host in wave 0, no earlier wave
    // can hold this dispatch. (Same-wave hosts do not count: that is the
    // budget gate's job.)
    let host_wave_idx = match host_wave {
        Some(0) | None => return None,
        Some(n) => n,
    };

    let rollout = input.rollout?;
    let waves = input.fleet.waves.get(host_channel)?;

    let earlier = outstanding_failures_in_waves(input.observed, &rollout.id, waves, 0..host_wave_idx);
    let failing_count: usize = earlier.iter().map(|(_, n)| *n).sum();

    if failing_count > 0 {
        Some(GateBlock::ComplianceWave {
            failing_events_count: failing_count,
            host_wave: host_wave_idx as u32,
        })
    } else {
        None
    }
}
