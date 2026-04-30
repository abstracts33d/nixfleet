//! Dispatch decision: route hosts to their CI-evaluated target.
//! Pure (no I/O, clock injected); caller handles DB side effects.
//!
//! 3-way compare: host's current generation, host's declared target,
//! and whether a `pending_confirms` row is in flight. The reconciler
//! emits the richer `Action` stream (waves, soaking, halts) for
//! observability; per-host dispatch is a direct comparison.

use chrono::{DateTime, Utc};

use nixfleet_proto::{
    agent_wire::{ActivateBlock, CheckinRequest, EvaluatedTarget, FetchResult},
    FleetResolved,
};

const CONFIRM_ENDPOINT: &str = "/v1/agent/confirm";

/// Canonical rollout-id derivation. Three CP sites must agree
/// (`decide_target`, `try_recover_orphan_confirm`,
/// `recover_soak_state_from_attestation`); drift silently splits
/// per-rollout grouping and breaks resolution-by-replacement.
///
/// Format `<channel>@<short>`. `Some("")` ci_commit yields
/// `channel@unknown` (operator config error); `None` falls back to
/// the closure prefix (legitimate for fleets without CI metadata,
/// e.g. test fixtures). The two are NOT equivalent.
pub fn derive_rollout_id(channel: &str, ci_commit: Option<&str>, target_closure: &str) -> String {
    fn truncate8(s: &str) -> String {
        let t: String = s.chars().take(8).collect();
        if t.is_empty() {
            "unknown".to_string()
        } else {
            t
        }
    }
    let suffix = ci_commit
        .map(truncate8)
        .unwrap_or_else(|| truncate8(target_closure));
    format!("{channel}@{suffix}")
}

/// `PartialEq` is intentionally NOT derived: `EvaluatedTarget`
/// doesn't implement it, and `evaluated_at` equality wouldn't be
/// meaningful. Tests pattern-match directly.
#[derive(Debug, Clone)]
pub enum Decision {
    Converged,
    /// Not in `fleet.resolved.hosts`.
    Unmanaged,
    /// Listed but no `closureHash` (CI didn't produce one).
    NoDeclaration,
    /// `pending_confirms` already in flight; don't re-dispatch.
    InFlight,
    /// Last fetch failed; hold rather than blast another target.
    HoldAfterFailure,
    Dispatch {
        target: EvaluatedTarget,
        rollout_id: String,
        wave_index: Option<u32>,
    },
}

/// Pure: caller passes `pending_for_host` (DB query result) and
/// `confirm_window_secs` (CP-side constant) so this function has
/// no I/O and is trivially unit-testable.
pub fn decide_target(
    hostname: &str,
    request: &CheckinRequest,
    fleet: &FleetResolved,
    pending_for_host: bool,
    now: DateTime<Utc>,
    confirm_window_secs: u32,
) -> Decision {
    let host = match fleet.hosts.get(hostname) {
        Some(h) => h,
        None => return Decision::Unmanaged,
    };

    let target_closure = match host.closure_hash.as_ref() {
        Some(h) => h,
        None => return Decision::NoDeclaration,
    };

    if request.current_generation.closure_hash == *target_closure {
        return Decision::Converged;
    }

    if pending_for_host {
        return Decision::InFlight;
    }

    if let Some(outcome) = &request.last_fetch_outcome {
        if matches!(
            outcome.result,
            FetchResult::VerifyFailed | FetchResult::FetchFailed
        ) {
            return Decision::HoldAfterFailure;
        }
    }

    let rollout_id = derive_rollout_id(
        &host.channel,
        fleet.meta.ci_commit.as_deref(),
        target_closure,
    );

    let wave_index: Option<u32> = fleet.waves.get(&host.channel).and_then(|waves| {
        waves
            .iter()
            .position(|w| w.hosts.iter().any(|h| h == hostname))
            .map(|i| i as u32)
    });

    // Relay so the agent runs a defense-in-depth freshness check.
    // Optional for forward-compat with older agent schemas; absent
    // fields fail open on the agent side.
    let signed_at = fleet.meta.signed_at;
    let freshness_window_secs = fleet
        .channels
        .get(&host.channel)
        .map(|ch| ch.freshness_window.saturating_mul(60));

    Decision::Dispatch {
        target: EvaluatedTarget {
            closure_hash: target_closure.clone(),
            channel_ref: rollout_id.clone(),
            evaluated_at: now,
            rollout_id: Some(rollout_id.clone()),
            wave_index,
            activate: Some(ActivateBlock {
                confirm_window_secs,
                confirm_endpoint: CONFIRM_ENDPOINT.to_string(),
            }),
            signed_at,
            freshness_window_secs,
            // — relay the channel's compliance mode so the
            // agent's runtime gate honours fleet-wide policy
            // pushes without needing per-host CLI flags. `None` only
            // on degenerate fleet-snapshot state where the channel
            // lookup itself misses; the wire field stays Optional
            // for backward compat with agents that pre-date it.
            compliance_mode: fleet
                .channels
                .get(&host.channel)
                .map(|ch| ch.compliance.mode.clone()),
        },
        rollout_id,
        wave_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nixfleet_proto::{
        agent_wire::{FetchOutcome, GenerationRef},
        fleet_resolved::Meta,
        Channel, Compliance, Host,
    };
    use std::collections::HashMap;

    #[test]
    fn derive_rollout_id_uses_ci_commit_prefix_when_present() {
        // Long ci_commit truncated to 8 chars; closure ignored.
        assert_eq!(
            derive_rollout_id("stable", Some("abc12345deadbeef"), "ignored-closure"),
            "stable@abc12345"
        );
    }

    #[test]
    fn derive_rollout_id_falls_back_to_closure_prefix_when_ci_commit_absent() {
        assert_eq!(
            derive_rollout_id("stable", None, "closurehash1234567890"),
            "stable@closureh"
        );
    }

    #[test]
    fn derive_rollout_id_substitutes_unknown_for_empty_ci_commit() {
        // — `Some("")` is operator misconfiguration, not
        // legitimate "no CI metadata". Surface as `channel@unknown`
        // rather than silently falling through to the closure.
        assert_eq!(
            derive_rollout_id("stable", Some(""), "closurehash1234"),
            "stable@unknown"
        );
    }

    #[test]
    fn derive_rollout_id_substitutes_unknown_when_both_sources_empty() {
        assert_eq!(derive_rollout_id("stable", None, ""), "stable@unknown");
        assert_eq!(derive_rollout_id("stable", Some(""), ""), "stable@unknown");
    }

    #[test]
    fn derive_rollout_id_handles_short_ci_commit_and_closure() {
        // Less than 8 chars — no padding, just take what's there.
        assert_eq!(
            derive_rollout_id("stable", Some("abc"), "closurehash"),
            "stable@abc"
        );
        assert_eq!(
            derive_rollout_id("stable", None, "abc"),
            "stable@abc"
        );
    }


    fn fleet_with(hostname: &str, host: Host) -> FleetResolved {
        let mut hosts = HashMap::new();
        hosts.insert(hostname.to_string(), host);
        let mut channels = HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "default".to_string(),
                reconcile_interval_minutes: 5,
                freshness_window: 60,
                signing_interval_minutes: 30,
                compliance: Compliance {
                    frameworks: vec![],
                    mode: "disabled".to_string(),
                },
            },
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies: HashMap::new(),
            waves: HashMap::new(),
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: Some(
                    DateTime::parse_from_rfc3339("2026-04-26T00:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                ci_commit: Some("abc12345deadbeef".to_string()),
                signature_algorithm: None,
            },
        }
    }

    fn host(closure_hash: Option<&str>) -> Host {
        Host {
            system: "x86_64-linux".to_string(),
            tags: vec![],
            channel: "stable".to_string(),
            closure_hash: closure_hash.map(String::from),
            pubkey: None,
        }
    }

    fn checkin(closure_hash: &str, fetch: Option<FetchResult>) -> CheckinRequest {
        CheckinRequest {
            hostname: "test-host".to_string(),
            agent_version: "test".to_string(),
            current_generation: GenerationRef {
                closure_hash: closure_hash.to_string(),
                channel_ref: None,
                boot_id: "boot".to_string(),
            },
            pending_generation: None,
            last_evaluated_target: None,
            last_fetch_outcome: fetch.map(|r| FetchOutcome {
                result: r,
                error: None,
            }),
            uptime_secs: None,
        last_confirmed_at: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn unmanaged_when_host_not_in_fleet() {
        let fleet = fleet_with("ohm", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, false, now(), 120),
            Decision::Unmanaged
        ));
    }

    #[test]
    fn no_declaration_when_fleet_omits_closure() {
        let fleet = fleet_with("test-host", host(None));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, false, now(), 120),
            Decision::NoDeclaration
        ));
    }

    #[test]
    fn converged_when_current_matches_target() {
        let fleet = fleet_with("test-host", host(Some("matched-system")));
        let req = checkin("matched-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, false, now(), 120),
            Decision::Converged
        ));
    }

    #[test]
    fn in_flight_when_pending_row_exists() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, /* pending */ true, now(), 120),
            Decision::InFlight
        ));
    }

    #[test]
    fn hold_after_verify_failed() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::VerifyFailed));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, false, now(), 120),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn hold_after_fetch_failed() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::FetchFailed));
        assert!(matches!(
            decide_target("test-host", &req, &fleet, false, now(), 120),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn dispatch_when_diverged_and_no_pending() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("test-host", &req, &fleet, false, now(), 120);
        let Decision::Dispatch {
            target,
            rollout_id,
            wave_index,
        } = d
        else {
            panic!("expected Dispatch, got {:?}", d);
        };
        assert_eq!(target.closure_hash, "declared-system");
        // ci_commit "abc12345deadbeef" → first 8 = "abc12345"
        assert_eq!(rollout_id, "stable@abc12345");
        assert_eq!(target.channel_ref, "stable@abc12345");
        assert_eq!(target.evaluated_at, now());
        // Wire-additive fields:
        assert_eq!(target.rollout_id.as_deref(), Some("stable@abc12345"));
        // No waves declared in fleet_with → both Decision and target
        // surface `wave_index = None`.
        assert_eq!(wave_index, None);
        assert_eq!(target.wave_index, None);
        let activate = target.activate.expect("activate block populated");
        assert_eq!(activate.confirm_window_secs, 120);
        assert_eq!(activate.confirm_endpoint, "/v1/agent/confirm");
    }

    #[test]
    fn dispatch_surfaces_wave_index_when_waves_declared() {
        // Channel has a 2-wave plan; the host is in wave 1 (index).
        let mut fleet = fleet_with("test-host", host(Some("declared-system")));
        fleet.waves.insert(
            "stable".to_string(),
            vec![
                nixfleet_proto::Wave {
                    hosts: vec!["other-host".to_string()],
                    soak_minutes: 5,
                },
                nixfleet_proto::Wave {
                    hosts: vec!["test-host".to_string()],
                    soak_minutes: 5,
                },
            ],
        );
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("test-host", &req, &fleet, false, now(), 120);
        let Decision::Dispatch {
            target, wave_index, ..
        } = d
        else {
            panic!("expected Dispatch");
        };
        assert_eq!(wave_index, Some(1));
        assert_eq!(target.wave_index, Some(1));
    }

    #[test]
    fn dispatch_falls_back_to_closure_hash_when_no_ci_commit() {
        let mut fleet = fleet_with("test-host", host(Some("xxxxxxxxyyyyyyy-system")));
        fleet.meta.ci_commit = None;
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("test-host", &req, &fleet, false, now(), 120);
        let Decision::Dispatch { rollout_id, .. } = d else {
            panic!("expected Dispatch");
        };
        assert_eq!(rollout_id, "stable@xxxxxxxx");
    }

    #[test]
    fn dispatch_threads_confirm_window_into_activate_block() {
        // Different confirm-window must propagate to the wire.
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("test-host", &req, &fleet, false, now(), 240);
        let Decision::Dispatch { target, .. } = d else {
            panic!("expected Dispatch");
        };
        let activate = target.activate.expect("activate block populated");
        assert_eq!(activate.confirm_window_secs, 240);
    }

    #[test]
    fn dispatch_when_no_fetch_outcome_yet() {
        // Brand-new agent, never fetched anything — should still dispatch.
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", None);
        let d = decide_target("test-host", &req, &fleet, false, now(), 120);
        assert!(matches!(d, Decision::Dispatch { .. }));
    }
}
