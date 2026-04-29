//! Wave-staging compliance gate (issue #59).
//!
//! Pure decision: given a host's report buffer, the channel's
//! compliance mode, and the host's current generation, return whether
//! dispatch should be blocked because outstanding `ComplianceFailure`
//! / `RuntimeGateError` events have not been resolved.
//!
//! ## Resolution semantics
//!
//! An event is **outstanding** until the host has moved on to a
//! strictly newer closure than the one the event was bound to. The
//! "newness" check uses `rollout` (the dispatch identifier the agent
//! echoes back on confirm) — events with `rollout != current_generation
//! rollout` are considered resolved-by-replacement (the host upgraded
//! past the failing closure).
//!
//! A `Mismatch` or `Malformed` signature status disqualifies the
//! event from the gate (see `evidence_verify::SignatureStatus::counts_for_gate`):
//! an attacker who can forge a sig can't grief the rollout by
//! injecting fake FAIL events for a host they've compromised the
//! mTLS cert of. Real FAIL events posted with valid signatures or
//! no signatures (legacy / no-pubkey) DO count.
//!
//! ## Per-wave scope
//!
//! The gate is per-wave (issue E in the cycle quality pass). A
//! host on wave N with outstanding events blocks dispatch only to
//! hosts on waves > N. Hosts on wave N or earlier still get
//! dispatched — they're either the failing host itself (which is
//! free to receive a *fixed* closure under operator control, since
//! that's the resolution path) or other hosts in the same wave
//! that haven't yet been dispatched.
//!
//! Earlier revisions blocked the entire channel on any failing
//! host. That over-aggressive semantic mattered nothing for the
//! lab's single-wave canary but would freeze a real fleet's
//! wave-0 progress on a wave-0 partial failure — exactly the
//! opposite of "wave-staging".
//!
//! Permissive mode never blocks dispatch; events are still recorded.

use nixfleet_proto::agent_wire::ReportEvent;
use nixfleet_proto::compliance::GateMode;

use crate::server::ReportRecord;

/// Returns true iff this report record carries a compliance failure
/// that counts toward the wave-staging gate.
fn record_is_compliance_failure(record: &ReportRecord) -> bool {
    let is_fail_event = matches!(
        record.report.event,
        ReportEvent::ComplianceFailure { .. } | ReportEvent::RuntimeGateError { .. }
    );
    if !is_fail_event {
        return false;
    }
    // Tampered events don't gate the rollout (defense vs. an
    // attacker forging FAIL events to block deploys). All other
    // statuses — Verified, Unsigned, NoPubkey, WrongAlgorithm —
    // count.
    match record.signature_status.as_ref() {
        Some(status) => status.counts_for_gate(),
        None => true,
    }
}

/// Filter a host's report ring buffer down to outstanding compliance
/// failures relative to `current_rollout`. An event is outstanding
/// if its `rollout` matches the host's current rollout — i.e. the
/// host is still running the closure the failure was reported for.
/// Events with `rollout != current_rollout` are resolved-by-
/// replacement (the host upgraded past the failing closure).
///
/// `current_rollout` is `None` when the host has never been seen on
/// this channel under a wave-aware dispatch (legacy or first
/// checkin) — in that case all failure events are treated as
/// outstanding (conservative: assume not-yet-resolved).
pub fn outstanding_failures<'a>(
    records: &'a [ReportRecord],
    current_rollout: Option<&str>,
) -> Vec<&'a ReportRecord> {
    records
        .iter()
        .filter(|r| record_is_compliance_failure(r))
        .filter(|r| match (current_rollout, r.report.rollout.as_deref()) {
            // Host has moved on to a newer rollout than the event's
            // rollout → event resolved.
            (Some(cur), Some(ev_r)) if cur != ev_r => false,
            // Host's current rollout matches the event's rollout, or
            // we don't know the host's current rollout — outstanding.
            _ => true,
        })
        .collect()
}

/// Verdict from `evaluate_channel_gate`. Reasoning is exposed for
/// journal logging + operator visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveGateOutcome {
    /// Channel mode is `disabled` (or compliance.mode=null and
    /// strict=false); gate did not run.
    NotApplicable,
    /// Channel mode is `permissive`; events recorded but never
    /// block dispatch.
    Permissive { failing_events_count: usize },
    /// Channel mode is `enforce` and no host on the channel has
    /// outstanding events; dispatch may proceed.
    EnforcePass,
    /// Channel mode is `enforce` and at least one host on the
    /// channel has outstanding events; dispatch blocked.
    EnforceBlock {
        failing_hosts: Vec<String>,
        failing_events_count: usize,
    },
}

impl WaveGateOutcome {
    /// True iff dispatch should be blocked at the wave level.
    pub fn blocks(&self) -> bool {
        matches!(self, WaveGateOutcome::EnforceBlock { .. })
    }
}

/// One host's contribution to the channel gate decision: its name,
/// its report buffer, the rollout id the host is currently
/// converged on (used for resolution-by-replacement), and its
/// wave index in the channel's rollout policy. `requesting_wave`
/// (the wave the dispatch decision is for) is compared against
/// this `wave_index` to decide whether the host's outstanding
/// events apply.
pub struct HostGateInput<'a> {
    pub hostname: &'a str,
    pub records: &'a [ReportRecord],
    pub current_rollout: Option<&'a str>,
    /// 0-based wave the host belongs to in
    /// `fleet.waves[channel]`. `None` for fleets without a wave
    /// plan (the lab's single-wave canary stays `None`); under
    /// `None`, every host counts toward the gate (no wave
    /// granularity available).
    pub wave_index: Option<u32>,
}

/// Compute the channel-level gate verdict for a dispatch decision.
///
/// `mode` is the channel's already-resolved `GateMode`. `requesting_wave`
/// is the wave the dispatch is being decided for (the wave of the
/// host whose checkin is being processed). `hosts` is every host on
/// the channel + its report buffer + its wave assignment.
///
/// Per-wave semantic (issue E): a failing host on wave N blocks
/// dispatch only to hosts on waves > N. Same-wave or earlier-wave
/// dispatches go through. When wave assignment is unknown
/// (`None` either side), the gate falls back to channel-wide
/// blocking — conservative under uncertainty.
pub fn evaluate_channel_gate<'a, I>(
    mode: GateMode,
    requesting_wave: Option<u32>,
    hosts: I,
) -> WaveGateOutcome
where
    I: IntoIterator<Item = HostGateInput<'a>>,
{
    if matches!(mode, GateMode::Disabled) {
        return WaveGateOutcome::NotApplicable;
    }

    let mut failing_hosts: Vec<String> = Vec::new();
    let mut failing_events_count = 0usize;

    for host in hosts {
        let outstanding = outstanding_failures(host.records, host.current_rollout);
        if outstanding.is_empty() {
            continue;
        }
        // Per-wave decision: a failing host on wave H blocks
        // dispatch only when the requesting host's wave is
        // strictly greater (we're trying to advance past H).
        // Same wave / earlier wave / unknown-on-either-side falls
        // back to "counts for the gate".
        let counts_for_request = match (requesting_wave, host.wave_index) {
            (Some(req), Some(h)) => req > h,
            // Either side unknown → conservative, count toward gate.
            _ => true,
        };
        if counts_for_request {
            failing_hosts.push(host.hostname.to_string());
            failing_events_count += outstanding.len();
        }
    }

    match mode {
        GateMode::Disabled => WaveGateOutcome::NotApplicable, // unreachable; covered above
        GateMode::Permissive => WaveGateOutcome::Permissive {
            failing_events_count,
        },
        GateMode::Enforce if failing_hosts.is_empty() => WaveGateOutcome::EnforcePass,
        GateMode::Enforce => WaveGateOutcome::EnforceBlock {
            failing_hosts,
            failing_events_count,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence_verify::SignatureStatus;
    use chrono::Utc;
    use nixfleet_proto::agent_wire::{ReportEvent, ReportRequest};

    fn make_record(event: ReportEvent, rollout: Option<&str>, sig: Option<SignatureStatus>) -> ReportRecord {
        ReportRecord {
            event_id: "evt-test".into(),
            received_at: Utc::now(),
            report: ReportRequest {
                hostname: "lab".into(),
                agent_version: "test".into(),
                occurred_at: Utc::now(),
                rollout: rollout.map(String::from),
                event,
            },
            signature_status: sig,
        }
    }

    fn compliance_failure(rollout: Option<&str>, sig: Option<SignatureStatus>) -> ReportRecord {
        make_record(
            ReportEvent::ComplianceFailure {
                control_id: "auditLogging".into(),
                status: "non-compliant".into(),
                framework_articles: vec![],
                evidence_snippet: None,
                evidence_collected_at: Utc::now(),
                signature: None,
            },
            rollout,
            sig,
        )
    }

    fn unrelated_event(rollout: Option<&str>) -> ReportRecord {
        make_record(
            ReportEvent::ActivationStarted {
                closure_hash: "x".into(),
                channel_ref: "edge-slow".into(),
            },
            rollout,
            None,
        )
    }

    #[test]
    fn outstanding_excludes_non_failure_events() {
        let records = vec![unrelated_event(Some("R1"))];
        assert!(outstanding_failures(&records, Some("R1")).is_empty());
    }

    #[test]
    fn outstanding_includes_failure_for_current_rollout() {
        let records = vec![compliance_failure(Some("R1"), None)];
        assert_eq!(outstanding_failures(&records, Some("R1")).len(), 1);
    }

    #[test]
    fn outstanding_excludes_failure_for_different_rollout() {
        let records = vec![compliance_failure(Some("R0"), None)];
        // Host has moved on to R1; R0's failure is resolved.
        assert!(outstanding_failures(&records, Some("R1")).is_empty());
    }

    #[test]
    fn outstanding_includes_failure_when_current_rollout_unknown() {
        let records = vec![compliance_failure(Some("R0"), None)];
        // First checkin / legacy: assume outstanding.
        assert_eq!(outstanding_failures(&records, None).len(), 1);
    }

    #[test]
    fn outstanding_excludes_tampered_events() {
        let records = vec![
            compliance_failure(Some("R1"), Some(SignatureStatus::Mismatch)),
            compliance_failure(Some("R1"), Some(SignatureStatus::Malformed)),
        ];
        assert!(outstanding_failures(&records, Some("R1")).is_empty());
    }

    #[test]
    fn outstanding_includes_verified_unsigned_nopubkey() {
        let records = vec![
            compliance_failure(Some("R1"), Some(SignatureStatus::Verified)),
            compliance_failure(Some("R1"), Some(SignatureStatus::Unsigned)),
            compliance_failure(Some("R1"), Some(SignatureStatus::NoPubkey)),
        ];
        assert_eq!(outstanding_failures(&records, Some("R1")).len(), 3);
    }

    fn host_input<'a>(
        hostname: &'a str,
        records: &'a [ReportRecord],
        current_rollout: Option<&'a str>,
        wave_index: Option<u32>,
    ) -> HostGateInput<'a> {
        HostGateInput {
            hostname,
            records,
            current_rollout,
            wave_index,
        }
    }

    #[test]
    fn evaluate_disabled_returns_not_applicable() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            GateMode::Disabled,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert_eq!(r, WaveGateOutcome::NotApplicable);
        assert!(!r.blocks());
    }

    #[test]
    fn evaluate_permissive_never_blocks() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            GateMode::Permissive,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert_eq!(
            r,
            WaveGateOutcome::Permissive { failing_events_count: 1 }
        );
        assert!(!r.blocks());
    }

    #[test]
    fn evaluate_enforce_blocks_on_outstanding_failures() {
        let records = vec![compliance_failure(Some("R1"), None)];
        // No wave info → conservative fallback: failing host counts.
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert!(r.blocks());
        if let WaveGateOutcome::EnforceBlock { failing_hosts, failing_events_count } = r {
            assert_eq!(failing_hosts, vec!["lab".to_string()]);
            assert_eq!(failing_events_count, 1);
        } else {
            panic!("expected EnforceBlock");
        }
    }

    #[test]
    fn evaluate_enforce_passes_when_no_failures() {
        let records: Vec<ReportRecord> = vec![];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert_eq!(r, WaveGateOutcome::EnforcePass);
        assert!(!r.blocks());
    }

    #[test]
    fn evaluate_enforce_resolved_by_replacement() {
        // Host posted a failure for R0, then upgraded to R1 cleanly.
        // No outstanding events under enforce → pass.
        let records = vec![compliance_failure(Some("R0"), None)];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert_eq!(r, WaveGateOutcome::EnforcePass);
    }

    #[test]
    fn evaluate_enforce_aggregates_multiple_hosts() {
        let host_a_records = vec![compliance_failure(Some("R1"), None)];
        let host_b_records: Vec<ReportRecord> = vec![];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            [
                host_input("host-a", &host_a_records, Some("R1"), None),
                host_input("host-b", &host_b_records, Some("R1"), None),
            ],
        );
        assert!(r.blocks());
        if let WaveGateOutcome::EnforceBlock { failing_hosts, .. } = r {
            assert_eq!(failing_hosts, vec!["host-a".to_string()]);
        }
    }

    #[test]
    fn evaluate_per_wave_blocks_only_later_waves() {
        // Wave-0 host failed; wave-1 dispatch should be blocked,
        // wave-0 dispatch should NOT be blocked (same-wave or
        // earlier-wave dispatches keep flowing — only promotion
        // past the failing wave is held).
        let failing = vec![compliance_failure(Some("R1"), None)];
        let inputs = || {
            vec![
                host_input("wave0-fail", &failing, Some("R1"), Some(0)),
                host_input("wave0-ok", &[], Some("R1"), Some(0)),
                host_input("wave1-target", &[], Some("R1"), Some(1)),
            ]
        };

        // Dispatch decision for a wave-0 host: should NOT block.
        let r0 = evaluate_channel_gate(GateMode::Enforce, Some(0), inputs());
        assert_eq!(r0, WaveGateOutcome::EnforcePass);

        // Dispatch decision for a wave-1 host: SHOULD block (the
        // wave-0 failure holds wave-1 promotion).
        let r1 = evaluate_channel_gate(GateMode::Enforce, Some(1), inputs());
        assert!(r1.blocks(), "wave-1 dispatch must block on wave-0 failure");
    }

    #[test]
    fn evaluate_per_wave_unknown_request_falls_back_conservative() {
        // Wave-0 failure; requesting wave is unknown — conservative
        // path counts the failure (block).
        let failing = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("wave0-fail", &failing, Some("R1"), Some(0))),
        );
        assert!(r.blocks());
    }

    // Earlier revisions had `evaluate_unknown_mode_falls_back_to_not_applicable`
    // and `evaluate_none_mode_falls_back_to_not_applicable` here. With
    // the move to a typed `GateMode` enum (issue D in the cycle quality
    // pass), unknown / unset modes can no longer reach this function
    // — they're collapsed at parse time by `GateMode::from_wire_str`
    // and `GateMode::resolve` upstream. The forward-compat behaviour
    // (unknown wire string → Permissive) is now tested in
    // `nixfleet-proto::compliance::tests`.
}
