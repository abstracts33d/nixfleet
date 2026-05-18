//! Wave-promotion gate (new-shape). Host's wave index must not exceed
//! the rollout's `current_wave`. Wave index comes from the verified
//! `FleetResolved.waves[channel]` (positional). `current_wave` lives on
//! `RolloutSummary` and is maintained by the applier.
//!
//! **Default-deny on inconsistent inputs** (D-027 hardening): the
//! pre-hardening gate silently passed when host-wave resolution
//! returned None (host not listed in any wave for its channel),
//! masking operator misconfiguration. Post-hardening, an unstaged host
//! in a channel that declares waves is blocked with
//! `GateBlock::HostUnstaged`. The "channel has no waves at all"
//! shape (typical of all-at-once policies; mk-fleet builds
//! `fleet.waves[channel] = []` when the policy declares no explicit
//! waves) still passes — that's intentional "no wave gating", not
//! misconfiguration.

use crate::planner_gates::GateBlock;
use crate::planner_types::{FleetState, HostId, RolloutId, SignedManifestSet};

pub fn check(
    fleet_state: &FleetState,
    manifests: &SignedManifestSet,
    host: &HostId,
    rollout_id: &RolloutId,
) -> Option<GateBlock> {
    let fleet = manifests.fleet();
    let channel = fleet.hosts.get(host).map(|h| h.channel.as_str())?;

    // Lookup `fleet.waves[channel]`.
    //   None or Some([]) → channel has no wave structure (typical of
    //                      all-at-once policies); no gating to apply.
    //   Some(non-empty) but host not in any wave → operator
    //                      misconfiguration; default-deny.
    //   Some(non-empty) and host in wave N → normal wave-promotion
    //                      check against current_wave.
    let waves = match fleet.waves.get(channel) {
        Some(w) if !w.is_empty() => w,
        _ => return None,
    };
    let host_wave = match waves
        .iter()
        .position(|w| w.hosts.iter().any(|h| h == host))
        .map(|i| i as u32)
    {
        Some(idx) => idx,
        None => {
            // D-027 hardening: a channel with declared waves but no
            // assignment for this host is a misconfiguration. Block
            // dispatch rather than silently passing.
            return Some(GateBlock::HostUnstaged {
                channel: channel.to_string(),
            });
        }
    };

    let current_wave = fleet_state
        .rollouts
        .get(rollout_id)
        .map(|r| r.current_wave)
        .unwrap_or(0);

    if host_wave > current_wave {
        Some(GateBlock::WavePromotion {
            host_wave,
            current_wave,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_types::{FleetState, RolloutSummary};
    use crate::verify::Verified;
    use nixfleet_proto::testing::FleetBuilder;
    use std::collections::HashMap;

    fn fleet_state_with_rollout(
        rollout_id: &RolloutId,
        channel: &str,
        current_wave: u32,
    ) -> FleetState {
        let mut rollouts = HashMap::new();
        rollouts.insert(
            rollout_id.clone(),
            RolloutSummary {
                rollout_id: rollout_id.clone(),
                channel: channel.to_string(),
                target_ref: "test-ref".to_string(),
                opened_at: chrono::Utc::now(),
                terminal_at: None,
                current_wave,
                budgets: vec![],
            },
        );
        FleetState {
            host_states: HashMap::new(),
            active_rollout_per_channel: HashMap::new(),
            rollouts,
            outstanding_failing_enforce_probes: HashMap::new(),
        }
    }

    /// D-027 hardening regression: a channel that declares waves but
    /// where the host being evaluated is not listed in any of them
    /// MUST block with `HostUnstaged`. Pre-hardening this case
    /// silently passed (`position(...) → None → ?-early-return →
    /// gate returns None`), masking operator misconfiguration that
    /// would let an unstaged host dispatch outside the wave plan.
    #[test]
    fn host_not_in_any_wave_blocks_with_host_unstaged() {
        let fleet = FleetBuilder::new()
            .host("h1", "stable")
            .host("h2", "stable")
            .host("ghost-host", "stable")
            .wave("stable", &["h1"])
            .wave("stable", &["h2"])
            .build();
        let manifests = SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, chrono::Utc::now()),
            rollouts: HashMap::new(),
        };
        let rollout_id: RolloutId = "stable@ref".into();
        let fs = fleet_state_with_rollout(&rollout_id, "stable", 0);

        let block = check(&fs, &manifests, &"ghost-host".to_string(), &rollout_id);
        assert!(
            matches!(block, Some(GateBlock::HostUnstaged { ref channel }) if channel == "stable"),
            "host not in any wave MUST block with HostUnstaged, got {block:?}"
        );
    }

    /// Channel with no waves at all (typical all-at-once shape post
    /// mk-fleet build with empty policy.waves) passes — intentional
    /// "no wave gating", not a misconfiguration.
    #[test]
    fn channel_with_empty_waves_passes_silently() {
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let manifests = SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, chrono::Utc::now()),
            rollouts: HashMap::new(),
        };
        let rollout_id: RolloutId = "stable@ref".into();
        let fs = fleet_state_with_rollout(&rollout_id, "stable", 0);

        let block = check(&fs, &manifests, &"h1".to_string(), &rollout_id);
        assert!(
            block.is_none(),
            "all-at-once channel (no declared waves) must pass; got {block:?}"
        );
    }

    /// Wave-1 host blocks when current_wave is 0 (the canonical
    /// happy-path block).
    #[test]
    fn host_in_later_wave_blocks_when_current_wave_is_earlier() {
        let fleet = FleetBuilder::new()
            .host("h1", "stable")
            .host("h2", "stable")
            .wave("stable", &["h1"])
            .wave("stable", &["h2"])
            .build();
        let manifests = SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, chrono::Utc::now()),
            rollouts: HashMap::new(),
        };
        let rollout_id: RolloutId = "stable@ref".into();
        let fs = fleet_state_with_rollout(&rollout_id, "stable", 0);

        let block = check(&fs, &manifests, &"h2".to_string(), &rollout_id);
        assert!(matches!(
            block,
            Some(GateBlock::WavePromotion {
                host_wave: 1,
                current_wave: 0,
            })
        ));
    }

    /// Wave-0 host passes when current_wave is 0.
    #[test]
    fn host_in_current_wave_passes() {
        let fleet = FleetBuilder::new()
            .host("h1", "stable")
            .host("h2", "stable")
            .wave("stable", &["h1"])
            .wave("stable", &["h2"])
            .build();
        let manifests = SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, chrono::Utc::now()),
            rollouts: HashMap::new(),
        };
        let rollout_id: RolloutId = "stable@ref".into();
        let fs = fleet_state_with_rollout(&rollout_id, "stable", 0);

        let block = check(&fs, &manifests, &"h1".to_string(), &rollout_id);
        assert!(
            block.is_none(),
            "host in current_wave must pass; got {block:?}"
        );
    }
}
