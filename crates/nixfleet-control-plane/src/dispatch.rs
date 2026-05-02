//! Dispatch decision: route hosts to their CI-evaluated target.
//! Pure (no I/O, clock injected); caller handles DB side effects.
//!
//! 3-way compare: host's current generation, host's declared target,
//! and whether a `host_dispatch_state` row is in flight. The
//! reconciler emits the richer `Action` stream (waves, soaking,
//! halts) for observability; per-host dispatch is a direct
//! comparison.

use chrono::{DateTime, Utc};

use nixfleet_proto::{
    agent_wire::{ActivateBlock, CheckinRequest, EvaluatedTarget, FetchResult},
    FleetResolved,
};

const CONFIRM_ENDPOINT: &str = "/v1/agent/confirm";

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
    /// Operational dispatch already in flight; don't re-dispatch.
    InFlight,
    /// Last fetch failed; hold rather than blast another target.
    HoldAfterFailure,
    Dispatch {
        target: EvaluatedTarget,
        rollout_id: String,
        wave_index: Option<u32>,
    },
}

/// Pure: caller passes `pending_for_host` (DB query result),
/// `confirm_window_secs` (CP-side constant), and `fleet_resolved_hash`
/// (the SHA-256 of the canonical bytes of `fleet`, computed once by
/// the channel-refs poll and stashed in AppState). The hash anchors
/// the rolloutId derivation to the same signed snapshot the producer
/// projected from — a different snapshot at the same channel ref
/// would produce a different rolloutId, by design.
pub fn decide_target(
    hostname: &str,
    request: &CheckinRequest,
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
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

    // RolloutId is the content hash of the projected RolloutManifest
    // for this host's channel — same projection the producer (and
    // any auditor) recompute. Drift here breaks the wire promise that
    // every advertised rolloutId resolves to a manifest CI signed.
    let rollout_id = match nixfleet_reconciler::compute_rollout_id_for_channel(
        fleet,
        fleet_resolved_hash,
        &host.channel,
    ) {
        Ok(Some(id)) => id,
        // Channel has no host with a declared closure — same shape as
        // the legacy NoDeclaration path. Should not normally fire here
        // (we already short-circuited on host.closure_hash above), but
        // belt-and-braces.
        Ok(None) => return Decision::NoDeclaration,
        Err(err) => {
            tracing::error!(
                hostname = %hostname,
                error = ?err,
                "dispatch: compute_rollout_id_for_channel failed; holding",
            );
            return Decision::HoldAfterFailure;
        }
    };

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
        Channel, Compliance, HealthGate, Host, OnHealthFailure, RolloutPolicy,
    };
    use std::collections::HashMap;

    /// Fixed test hash — actual content irrelevant for unit tests; the
    /// production CP computes this from the canonical bytes of the
    /// verified fleet. Tests use a stable string so assertions on
    /// rolloutId equality work without caring about the hash itself.
    const TEST_FLEET_HASH: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";

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
        let mut rollout_policies = HashMap::new();
        rollout_policies.insert(
            "default".to_string(),
            RolloutPolicy {
                strategy: "waves".to_string(),
                waves: vec![],
                health_gate: HealthGate::default(),
                on_health_failure: OnHealthFailure::Halt,
            },
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies,
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
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                false,
                now(),
                120
            ),
            Decision::Unmanaged
        ));
    }

    #[test]
    fn no_declaration_when_fleet_omits_closure() {
        let fleet = fleet_with("test-host", host(None));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                false,
                now(),
                120
            ),
            Decision::NoDeclaration
        ));
    }

    #[test]
    fn converged_when_current_matches_target() {
        let fleet = fleet_with("test-host", host(Some("matched-system")));
        let req = checkin("matched-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                false,
                now(),
                120
            ),
            Decision::Converged
        ));
    }

    #[test]
    fn in_flight_when_pending_row_exists() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                /* pending */ true,
                now(),
                120
            ),
            Decision::InFlight
        ));
    }

    #[test]
    fn hold_after_verify_failed() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::VerifyFailed));
        assert!(matches!(
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                false,
                now(),
                120
            ),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn hold_after_fetch_failed() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::FetchFailed));
        assert!(matches!(
            decide_target(
                "test-host",
                &req,
                &fleet,
                TEST_FLEET_HASH,
                false,
                now(),
                120
            ),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn dispatch_when_diverged_and_no_pending() {
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target(
            "test-host",
            &req,
            &fleet,
            TEST_FLEET_HASH,
            false,
            now(),
            120,
        );
        let Decision::Dispatch {
            target,
            rollout_id,
            wave_index,
        } = d
        else {
            panic!("expected Dispatch, got {:?}", d);
        };
        assert_eq!(target.closure_hash, "declared-system");
        // rolloutId is now a 64-char hex content hash (sha256 over the
        // canonical bytes of the projected RolloutManifest). Exact
        // value depends on every field of the manifest projection;
        // we assert shape, not value.
        assert_eq!(rollout_id.len(), 64);
        assert!(rollout_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // channel_ref still mirrors the rolloutId on the wire (it's
        // the identifier the agent sends back on confirm).
        assert_eq!(target.channel_ref, rollout_id);
        assert_eq!(target.evaluated_at, now());
        assert_eq!(target.rollout_id.as_deref(), Some(rollout_id.as_str()));
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
        let d = decide_target(
            "test-host",
            &req,
            &fleet,
            TEST_FLEET_HASH,
            false,
            now(),
            120,
        );
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
    fn dispatch_yields_distinct_rollout_ids_for_distinct_snapshots() {
        // Two manifests projected from snapshots at different fleet
        // hashes produce different rolloutIds (the fleetResolvedHash
        // is part of the canonical surface). Validates the anchor
        // is load-bearing in dispatch's id derivation.
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d1 = decide_target(
            "test-host",
            &req,
            &fleet,
            "1111111111111111111111111111111111111111111111111111111111111111",
            false,
            now(),
            120,
        );
        let d2 = decide_target(
            "test-host",
            &req,
            &fleet,
            "2222222222222222222222222222222222222222222222222222222222222222",
            false,
            now(),
            120,
        );
        let (id1, id2) = match (d1, d2) {
            (
                Decision::Dispatch { rollout_id: a, .. },
                Decision::Dispatch { rollout_id: b, .. },
            ) => (a, b),
            other => panic!("expected two Dispatch decisions, got {other:?}"),
        };
        assert_ne!(id1, id2);
    }

    #[test]
    fn dispatch_threads_confirm_window_into_activate_block() {
        // Different confirm-window must propagate to the wire.
        let fleet = fleet_with("test-host", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target(
            "test-host",
            &req,
            &fleet,
            TEST_FLEET_HASH,
            false,
            now(),
            240,
        );
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
        let d = decide_target(
            "test-host",
            &req,
            &fleet,
            TEST_FLEET_HASH,
            false,
            now(),
            120,
        );
        assert!(matches!(d, Decision::Dispatch { .. }));
    }
}
