//! Dispatch loop — bridge from `fleet.resolved.json` (CI signed) to
//! `CheckinResponse.target` (agent activates).
//!
//! Per ARCHITECTURE.md the CP holds no opinions: it routes hosts to
//! their declared target as evaluated by CI. The decision per
//! checkin is a 3-way comparison:
//!
//! 1. The host's current generation (from `CheckinRequest`).
//! 2. The host's declared target (`fleet.resolved.hosts[h].closureHash`).
//! 3. Whether a `pending_confirms` row is already in flight.
//!
//! The reconciler crate (`nixfleet-reconciler`) emits a richer
//! `Action` stream — waves, soaking, halts — for log/observability.
//! Per-host dispatch here is a direct comparison; no reconciler state
//! machine is required to close the activation chain. When wave
//! staging is added, the wave/soak gates plug in *before* this
//! decision.
//!
//! The function in this module is pure: no I/O, clock injected. The
//! caller (the `/v1/agent/checkin` handler in `server.rs`) is
//! responsible for the DB lookup + insert side effects.

use chrono::{DateTime, Utc};

use nixfleet_proto::{
    agent_wire::{CheckinRequest, EvaluatedTarget, FetchResult},
    FleetResolved,
};

/// Outcome of the dispatch decision for a host.
///
/// `PartialEq` is intentionally NOT derived: `EvaluatedTarget`
/// doesn't implement it, and the equality semantics on a freshly-
/// allocated `evaluated_at` are not meaningful anyway. Tests pattern-
/// match the variants directly.
#[derive(Debug, Clone)]
pub enum Decision {
    /// Host already runs the declared target. Return `target: null`.
    Converged,
    /// Host is unknown to the fleet (`fleet.resolved.hosts` does not
    /// list it). The CP does not manage this host. Return `target: null`.
    Unmanaged,
    /// Host is listed but the fleet declares no `closureHash` for it
    /// (CI evaluation didn't produce one). Return `target: null`.
    NoDeclaration,
    /// A `pending_confirms` row is already in flight for this host
    /// (the agent is mid-activation, or the prior dispatch has not
    /// expired or rolled back yet). Don't re-dispatch.
    InFlight,
    /// Last fetch reported a verify or fetch failure. Hold rather
    /// than blast another target while the agent is still recovering.
    HoldAfterFailure,
    /// Dispatch this target.
    Dispatch {
        target: EvaluatedTarget,
        rollout_id: String,
    },
}

/// Pure dispatch decision.
///
/// `pending_for_host` is `true` if the DB has any `pending_confirms`
/// row in state `'pending'` for this hostname (regardless of which
/// rollout). The caller queries the DB and passes the bool — keeps
/// this function pure and trivially unit-testable.
pub fn decide_target(
    hostname: &str,
    request: &CheckinRequest,
    fleet: &FleetResolved,
    pending_for_host: bool,
    now: DateTime<Utc>,
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

    // Rollout id format: `<channel>@<short>` per RFC-0003 §4.2 example
    // (`stable@r2`). The suffix is the first 8 chars of the CI commit
    // when present, otherwise the first 8 of the closure hash. Both
    // are deterministic from `fleet.resolved` so two checkins of the
    // same fleet produce the same rollout id — required for idempotent
    // INSERT into `pending_confirms`.
    let suffix: String = fleet
        .meta
        .ci_commit
        .as_deref()
        .map(|c| c.chars().take(8).collect::<String>())
        .unwrap_or_else(|| target_closure.chars().take(8).collect::<String>());
    let rollout_id = format!("{}@{}", host.channel, suffix);

    Decision::Dispatch {
        target: EvaluatedTarget {
            closure_hash: target_closure.clone(),
            channel_ref: rollout_id.clone(),
            evaluated_at: now,
        },
        rollout_id,
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
                    strict: false,
                    frameworks: vec![],
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
            hostname: "krach".to_string(),
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
            decide_target("krach", &req, &fleet, false, now()),
            Decision::Unmanaged
        ));
    }

    #[test]
    fn no_declaration_when_fleet_omits_closure() {
        let fleet = fleet_with("krach", host(None));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("krach", &req, &fleet, false, now()),
            Decision::NoDeclaration
        ));
    }

    #[test]
    fn converged_when_current_matches_target() {
        let fleet = fleet_with("krach", host(Some("matched-system")));
        let req = checkin("matched-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("krach", &req, &fleet, false, now()),
            Decision::Converged
        ));
    }

    #[test]
    fn in_flight_when_pending_row_exists() {
        let fleet = fleet_with("krach", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        assert!(matches!(
            decide_target("krach", &req, &fleet, /* pending */ true, now()),
            Decision::InFlight
        ));
    }

    #[test]
    fn hold_after_verify_failed() {
        let fleet = fleet_with("krach", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::VerifyFailed));
        assert!(matches!(
            decide_target("krach", &req, &fleet, false, now()),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn hold_after_fetch_failed() {
        let fleet = fleet_with("krach", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::FetchFailed));
        assert!(matches!(
            decide_target("krach", &req, &fleet, false, now()),
            Decision::HoldAfterFailure
        ));
    }

    #[test]
    fn dispatch_when_diverged_and_no_pending() {
        let fleet = fleet_with("krach", host(Some("declared-system")));
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("krach", &req, &fleet, false, now());
        let Decision::Dispatch { target, rollout_id } = d else {
            panic!("expected Dispatch, got {:?}", d);
        };
        assert_eq!(target.closure_hash, "declared-system");
        // ci_commit "abc12345deadbeef" → first 8 = "abc12345"
        assert_eq!(rollout_id, "stable@abc12345");
        assert_eq!(target.channel_ref, "stable@abc12345");
        assert_eq!(target.evaluated_at, now());
    }

    #[test]
    fn dispatch_falls_back_to_closure_hash_when_no_ci_commit() {
        let mut fleet = fleet_with("krach", host(Some("xxxxxxxxyyyyyyy-system")));
        fleet.meta.ci_commit = None;
        let req = checkin("running-system", Some(FetchResult::Ok));
        let d = decide_target("krach", &req, &fleet, false, now());
        let Decision::Dispatch { rollout_id, .. } = d else {
            panic!("expected Dispatch");
        };
        assert_eq!(rollout_id, "stable@xxxxxxxx");
    }

    #[test]
    fn dispatch_when_no_fetch_outcome_yet() {
        // Brand-new agent, never fetched anything — should still dispatch.
        let fleet = fleet_with("krach", host(Some("declared-system")));
        let req = checkin("running-system", None);
        let d = decide_target("krach", &req, &fleet, false, now());
        assert!(matches!(d, Decision::Dispatch { .. }));
    }
}
