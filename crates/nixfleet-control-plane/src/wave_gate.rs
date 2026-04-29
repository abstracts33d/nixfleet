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
//! ## Per-channel scope
//!
//! The gate is per-channel: ANY host on channel C with outstanding
//! events blocks dispatch of NEW closures to ANY host on C (under
//! enforce mode). This is the wave-promotion semantic — wave N+1 is
//! held while wave N has unresolved compliance issues.
//!
//! Permissive mode never blocks dispatch; events are still recorded.

use nixfleet_proto::agent_wire::ReportEvent;

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

/// Compute the channel-level gate verdict.
///
/// `mode` is the channel's resolved compliance mode (`"disabled"` /
/// `"permissive"` / `"enforce"` / None). The legacy `strict` flag
/// is mapped to a mode upstream — this function works only with the
/// resolved mode string.
///
/// `host_reports_for_channel` is an iterator over `(hostname,
/// records, current_rollout)` for every host on the channel. The
/// caller is responsible for the lookup; this function stays pure.
pub fn evaluate_channel_gate<'a, I>(
    mode: Option<&str>,
    host_reports_for_channel: I,
) -> WaveGateOutcome
where
    I: IntoIterator<Item = (&'a str, &'a [ReportRecord], Option<&'a str>)>,
{
    let effective_mode = mode.unwrap_or("disabled");

    if effective_mode == "disabled" {
        return WaveGateOutcome::NotApplicable;
    }

    let mut failing_hosts: Vec<String> = Vec::new();
    let mut failing_events_count = 0usize;

    for (hostname, records, current_rollout) in host_reports_for_channel {
        let outstanding = outstanding_failures(records, current_rollout);
        if !outstanding.is_empty() {
            failing_hosts.push(hostname.to_string());
            failing_events_count += outstanding.len();
        }
    }

    match effective_mode {
        "permissive" => WaveGateOutcome::Permissive {
            failing_events_count,
        },
        "enforce" if failing_hosts.is_empty() => WaveGateOutcome::EnforcePass,
        "enforce" => WaveGateOutcome::EnforceBlock {
            failing_hosts,
            failing_events_count,
        },
        // Unknown mode strings fall back to disabled (forward-
        // compatibility with future modes the agent / proto might
        // add — never break the rollout because of a mode the CP
        // doesn't recognise).
        _ => WaveGateOutcome::NotApplicable,
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

    #[test]
    fn evaluate_disabled_returns_not_applicable() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            Some("disabled"),
            std::iter::once(("lab", &records[..], Some("R1"))),
        );
        assert_eq!(r, WaveGateOutcome::NotApplicable);
        assert!(!r.blocks());
    }

    #[test]
    fn evaluate_permissive_never_blocks() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            Some("permissive"),
            std::iter::once(("lab", &records[..], Some("R1"))),
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
        let r = evaluate_channel_gate(
            Some("enforce"),
            std::iter::once(("lab", &records[..], Some("R1"))),
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
            Some("enforce"),
            std::iter::once(("lab", &records[..], Some("R1"))),
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
            Some("enforce"),
            std::iter::once(("lab", &records[..], Some("R1"))),
        );
        assert_eq!(r, WaveGateOutcome::EnforcePass);
    }

    #[test]
    fn evaluate_enforce_aggregates_multiple_hosts() {
        // Two hosts on the channel. Host-A failed, host-B clean →
        // wave-staging blocks because of host-A.
        let host_a_records = vec![compliance_failure(Some("R1"), None)];
        let host_b_records: Vec<ReportRecord> = vec![];
        let r = evaluate_channel_gate(
            Some("enforce"),
            [
                ("host-a", &host_a_records[..], Some("R1")),
                ("host-b", &host_b_records[..], Some("R1")),
            ],
        );
        assert!(r.blocks());
        if let WaveGateOutcome::EnforceBlock { failing_hosts, .. } = r {
            assert_eq!(failing_hosts, vec!["host-a".to_string()]);
        }
    }

    #[test]
    fn evaluate_unknown_mode_falls_back_to_not_applicable() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            Some("future-mode"),
            std::iter::once(("lab", &records[..], Some("R1"))),
        );
        assert_eq!(r, WaveGateOutcome::NotApplicable);
    }

    #[test]
    fn evaluate_none_mode_falls_back_to_not_applicable() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            None,
            std::iter::once(("lab", &records[..], Some("R1"))),
        );
        assert_eq!(r, WaveGateOutcome::NotApplicable);
    }
}
