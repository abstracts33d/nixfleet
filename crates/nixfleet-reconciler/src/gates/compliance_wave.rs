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

use super::{GateBlock, GateInput};

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

    let per_host = input
        .observed
        .compliance_failures_by_rollout
        .get(&rollout.id)?;

    let waves = input.fleet.waves.get(host_channel)?;
    let mut failing_count: usize = 0;
    for earlier_wave in waves.iter().take(host_wave_idx) {
        for h in &earlier_wave.hosts {
            if let Some(n) = per_host.get(h) {
                failing_count = failing_count.saturating_add(*n);
            }
        }
    }

    if failing_count > 0 {
        Some(GateBlock::ComplianceWave {
            failing_events_count: failing_count,
            host_wave: host_wave_idx as u32,
        })
    } else {
        None
    }
}
