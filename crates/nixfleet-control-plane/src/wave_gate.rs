//! Pure wave-staging compliance gate; resolution-by-replacement, per-wave, signature-aware.

use nixfleet_proto::agent_wire::ReportEvent;
use nixfleet_proto::compliance::GateMode;

use crate::server::ReportRecord;

fn record_is_compliance_failure(record: &ReportRecord) -> bool {
    let is_fail_event = matches!(
        record.report.event,
        ReportEvent::ComplianceFailure { .. } | ReportEvent::RuntimeGateError { .. }
    );
    if !is_fail_event {
        return false;
    }
    match record.signature_status.as_ref() {
        Some(status) => status.counts_for_gate(),
        None => true,
    }
}

/// `None` current_rollout is conservative — all failure events count.
pub fn outstanding_failures<'a>(
    records: &'a [ReportRecord],
    current_rollout: Option<&str>,
) -> Vec<&'a ReportRecord> {
    records
        .iter()
        .filter(|r| record_is_compliance_failure(r))
        .filter(|r| {
            !matches!(
                (current_rollout, r.report.rollout.as_deref()),
                (Some(cur), Some(ev_r)) if cur != ev_r
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveGateOutcome {
    NotApplicable,
    Permissive { failing_events_count: usize },
    EnforcePass,
    EnforceBlock {
        failing_hosts: Vec<String>,
        failing_events_count: usize,
    },
}

impl WaveGateOutcome {
    pub fn blocks(&self) -> bool {
        matches!(self, WaveGateOutcome::EnforceBlock { .. })
    }
}

pub struct HostGateInput<'a> {
    pub hostname: &'a str,
    pub records: &'a [ReportRecord],
    pub current_rollout: Option<&'a str>,
    /// `None` (no wave plan) → every host counts toward the gate.
    pub wave_index: Option<u32>,
}

/// Failing host on wave N blocks only `requesting_wave > N`; unknown wave → conservative.
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
        let counts_for_request = match (requesting_wave, host.wave_index) {
            (Some(req), Some(h)) => req > h,
            _ => true,
        };
        if counts_for_request {
            failing_hosts.push(host.hostname.to_string());
            failing_events_count += outstanding.len();
        }
    }

    match mode {
        GateMode::Disabled => WaveGateOutcome::NotApplicable,
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
    use chrono::Utc;
    use nixfleet_proto::agent_wire::{ReportEvent, ReportRequest};
    use nixfleet_reconciler::evidence::SignatureStatus;

    fn make_record(
        event: ReportEvent,
        rollout: Option<&str>,
        sig: Option<SignatureStatus>,
    ) -> ReportRecord {
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
        assert!(outstanding_failures(&records, Some("R1")).is_empty());
    }

    #[test]
    fn outstanding_includes_failure_when_current_rollout_unknown() {
        let records = vec![compliance_failure(Some("R0"), None)];
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
            WaveGateOutcome::Permissive {
                failing_events_count: 1
            }
        );
        assert!(!r.blocks());
    }

    #[test]
    fn evaluate_enforce_blocks_on_outstanding_failures() {
        let records = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("lab", &records, Some("R1"), None)),
        );
        assert!(r.blocks());
        if let WaveGateOutcome::EnforceBlock {
            failing_hosts,
            failing_events_count,
        } = r
        {
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
        let failing = vec![compliance_failure(Some("R1"), None)];
        let inputs = || {
            vec![
                host_input("wave0-fail", &failing, Some("R1"), Some(0)),
                host_input("wave0-ok", &[], Some("R1"), Some(0)),
                host_input("wave1-target", &[], Some("R1"), Some(1)),
            ]
        };

        let r0 = evaluate_channel_gate(GateMode::Enforce, Some(0), inputs());
        assert_eq!(r0, WaveGateOutcome::EnforcePass);

        let r1 = evaluate_channel_gate(GateMode::Enforce, Some(1), inputs());
        assert!(r1.blocks(), "wave-1 dispatch must block on wave-0 failure");
    }

    #[test]
    fn evaluate_per_wave_unknown_request_falls_back_conservative() {
        let failing = vec![compliance_failure(Some("R1"), None)];
        let r = evaluate_channel_gate(
            GateMode::Enforce,
            None,
            std::iter::once(host_input("wave0-fail", &failing, Some("R1"), Some(0))),
        );
        assert!(r.blocks());
    }
}
