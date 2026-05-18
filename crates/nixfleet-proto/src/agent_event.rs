//! Wire-format types for `POST /v1/agent/events` (RFC-0008 §4.2).
//!
//! Lives in `nixfleet-proto` so the agent (producer) and CP (consumer)
//! share a single canonical definition. Prior to this lift the envelope
//! was hand-built as `serde_json::Map` on the agent and re-defined as a
//! Rust struct on the CP side; a casing mismatch on the outer
//! `rollout_id` field was the surface defect that exposed the
//! duplicated-definition shape. Lifted per RFC-0011 §2: any type that
//! crosses the agent <-> CP boundary lives in `nixfleet-proto`, not in
//! both sides simultaneously.
//!
//! Wire convention pinned here:
//!   - Envelope: outer fields are `camelCase` (`hostname`, `rolloutId`,
//!     `event`, optional `signature`).
//!   - Inner event: `tag = "kind"` (PascalCase variants) +
//!     `camelCase` field names for the variant payloads.
//!   - Probe-status / probe-mode / on-health-failure: keep their
//!     historic wire shapes (`lowercase`, `kebab-case`, `kebab-case`).
//!
//! Conversions to `nixfleet_state_machine` types live in that crate
//! (orphan rule); this module defines the wire surface only.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::RolloutId;

/// Outer envelope agents POST to `/v1/agent/events`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventEnvelope {
    pub hostname: String,
    pub rollout_id: RolloutId,
    pub event: AgentEvent,
    /// Hex-encoded Ed25519 signature over canonicalised `event` bytes.
    /// Optional in v0.2 (mTLS provides primary auth); enforced in
    /// Phase 7+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Inbound agent events. Mirrors the wire side of
/// `nixfleet_state_machine::OutboundAgentEvent` (same variant names,
/// `camelCase` fields per RFC-0008 §4.2).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    DispatchAck {
        current_closure_at_dispatch: String,
        received_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ActivationStarted {
        started_at: DateTime<Utc>,
        switch_method: String,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ActivationCompleted {
        observed_current_closure: String,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ActivationFailed {
        exit_code: i32,
        stderr_tail: String,
        failed_at: DateTime<Utc>,
        seq: u64,
    },
    /// LIFT #2 (RFC-0008 §4.2): live activation skipped because
    /// `component` (dbus/systemd/kernel/init) cannot be live-swapped on
    /// a running system. Profile + bootloader updated; next reboot
    /// completes the activation. Replaces the pre-LIFT-#2 fake
    /// `ActivationCompleted` with `exit_code: 0`.
    #[serde(rename_all = "camelCase")]
    ActivationDeferred {
        component: String,
        deferred_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ProbeTopologyDeclared {
        probes: Vec<ProbeTopologyEntryWire>,
        declared_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ProbeObservedFirst {
        probe_name: String,
        mode: ProbeModeWire,
        observed_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ProbeResult {
        probe_name: String,
        mode: ProbeModeWire,
        status: ProbeStatusWire,
        observed_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub_results: Option<Vec<ProbeSubResultWire>>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    ProbeFailureFirst {
        probe_name: String,
        mode: ProbeModeWire,
        first_failed_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    Failed {
        failed_at: DateTime<Utc>,
        sustained_duration_secs: u64,
        failing_probes: Vec<String>,
        policy_applied: OnHealthFailureWire,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    RollbackComplete {
        reverted_to_closure: String,
        exit_code: i32,
        completed_at: DateTime<Utc>,
        seq: u64,
    },
    #[serde(rename_all = "camelCase")]
    Converged {
        converged_at: DateTime<Utc>,
        current_closure: String,
        seq: u64,
    },
}

impl AgentEvent {
    pub fn seq(&self) -> u64 {
        match self {
            AgentEvent::DispatchAck { seq, .. }
            | AgentEvent::ActivationStarted { seq, .. }
            | AgentEvent::ActivationCompleted { seq, .. }
            | AgentEvent::ActivationDeferred { seq, .. }
            | AgentEvent::ActivationFailed { seq, .. }
            | AgentEvent::ProbeTopologyDeclared { seq, .. }
            | AgentEvent::ProbeObservedFirst { seq, .. }
            | AgentEvent::ProbeResult { seq, .. }
            | AgentEvent::ProbeFailureFirst { seq, .. }
            | AgentEvent::Failed { seq, .. }
            | AgentEvent::RollbackComplete { seq, .. }
            | AgentEvent::Converged { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStatusWire {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeModeWire {
    #[default]
    Enforce,
    Observe,
    Disabled,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OnHealthFailureWire {
    Halt,
    RollbackAndHalt,
}

impl From<OnHealthFailureWire> for crate::OnHealthFailure {
    fn from(w: OnHealthFailureWire) -> Self {
        match w {
            OnHealthFailureWire::Halt => crate::OnHealthFailure::Halt,
            OnHealthFailureWire::RollbackAndHalt => crate::OnHealthFailure::RollbackAndHalt,
        }
    }
}

impl From<crate::OnHealthFailure> for OnHealthFailureWire {
    fn from(p: crate::OnHealthFailure) -> Self {
        match p {
            crate::OnHealthFailure::Halt => OnHealthFailureWire::Halt,
            crate::OnHealthFailure::RollbackAndHalt => OnHealthFailureWire::RollbackAndHalt,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProbeTopologyEntryWire {
    pub probe_name: String,
    pub kind: String,
    pub mode: ProbeModeWire,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSubResultWire {
    pub control_id: String,
    pub status: ProbeStatusWire,
    pub framework: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article: Option<String>,
    /// Per-control effective mode resolved from the probe's
    /// `controlOverrides` / `controls` declaration. The CP applier
    /// inserts `probe_failures` rows only for `Enforce`-mode failures;
    /// `Observe` rows are recorded in `event_log` for visibility but
    /// do not gate the compliance_wave. `Disabled` controls are
    /// filtered out by the agent before emission.
    #[serde(default)]
    pub effective_mode: ProbeModeWire,
    /// Audit rationale from the operator's `controlOverrides[id].reason`
    /// (or `controls[id].reason`). Surfaces in CP's event_log so
    /// auditors can recover the "why was this control downgraded?"
    /// answer from the signed event stream alone — no out-of-band
    /// reference to fleet.nix needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 17, 12, 0, 0).unwrap()
    }

    fn envelope_with(event: AgentEvent) -> AgentEventEnvelope {
        AgentEventEnvelope {
            hostname: "host-test".into(),
            rollout_id: RolloutId::new("stable", "abc1234"),
            event,
            signature: None,
        }
    }

    fn round_trip(env: AgentEventEnvelope) {
        let raw = serde_json::to_string(&env).expect("serialize envelope");
        let back: AgentEventEnvelope = serde_json::from_str(&raw).expect("deserialize envelope");
        assert_eq!(env, back, "envelope round-trip preserves equality");
    }

    #[test]
    fn envelope_outer_keys_are_camelcase() {
        let env = envelope_with(AgentEvent::DispatchAck {
            current_closure_at_dispatch: "prior".into(),
            received_at: fixed_now(),
            seq: 1,
        });
        let raw = serde_json::to_string(&env).unwrap();
        assert!(
            raw.contains("\"rolloutId\""),
            "envelope must use camelCase rolloutId: {raw}",
        );
        assert!(
            !raw.contains("\"rollout_id\""),
            "envelope must NOT use snake_case rollout_id: {raw}",
        );
    }

    #[test]
    fn dispatch_ack_round_trip() {
        round_trip(envelope_with(AgentEvent::DispatchAck {
            current_closure_at_dispatch: "prior-closure".into(),
            received_at: fixed_now(),
            seq: 1,
        }));
    }

    #[test]
    fn activation_started_round_trip() {
        round_trip(envelope_with(AgentEvent::ActivationStarted {
            started_at: fixed_now(),
            switch_method: "systemd-run".into(),
            seq: 2,
        }));
    }

    #[test]
    fn activation_completed_round_trip() {
        round_trip(envelope_with(AgentEvent::ActivationCompleted {
            observed_current_closure: "closure-a".into(),
            exit_code: 0,
            completed_at: fixed_now(),
            seq: 3,
        }));
    }

    #[test]
    fn activation_failed_round_trip() {
        round_trip(envelope_with(AgentEvent::ActivationFailed {
            exit_code: 1,
            stderr_tail: "boom".into(),
            failed_at: fixed_now(),
            seq: 4,
        }));
    }

    #[test]
    fn probe_topology_declared_round_trip() {
        round_trip(envelope_with(AgentEvent::ProbeTopologyDeclared {
            probes: vec![ProbeTopologyEntryWire {
                probe_name: "nginx".into(),
                kind: "http".into(),
                mode: ProbeModeWire::Enforce,
            }],
            declared_at: fixed_now(),
            seq: 5,
        }));
    }

    #[test]
    fn probe_observed_first_round_trip() {
        round_trip(envelope_with(AgentEvent::ProbeObservedFirst {
            probe_name: "nginx".into(),
            mode: ProbeModeWire::Observe,
            observed_at: fixed_now(),
            seq: 6,
        }));
    }

    #[test]
    fn probe_result_round_trip_with_sub_results() {
        round_trip(envelope_with(AgentEvent::ProbeResult {
            probe_name: "evidence-nis2".into(),
            mode: ProbeModeWire::Enforce,
            status: ProbeStatusWire::Fail,
            observed_at: fixed_now(),
            failure_reason: Some("missing control".into()),
            sub_results: Some(vec![ProbeSubResultWire {
                control_id: "A.8.1".into(),
                status: ProbeStatusWire::Fail,
                framework: "nis2".into(),
                article: Some("21.2.h".into()),
                effective_mode: ProbeModeWire::Enforce,
                override_reason: None,
            }]),
            seq: 7,
        }));
    }

    #[test]
    fn probe_failure_first_round_trip() {
        round_trip(envelope_with(AgentEvent::ProbeFailureFirst {
            probe_name: "nginx".into(),
            mode: ProbeModeWire::Enforce,
            first_failed_at: fixed_now(),
            seq: 8,
        }));
    }

    #[test]
    fn failed_round_trip() {
        round_trip(envelope_with(AgentEvent::Failed {
            failed_at: fixed_now(),
            sustained_duration_secs: 120,
            failing_probes: vec!["nginx".into()],
            policy_applied: OnHealthFailureWire::RollbackAndHalt,
            seq: 9,
        }));
    }

    #[test]
    fn rollback_complete_round_trip() {
        round_trip(envelope_with(AgentEvent::RollbackComplete {
            reverted_to_closure: "prior-closure".into(),
            exit_code: 0,
            completed_at: fixed_now(),
            seq: 10,
        }));
    }

    #[test]
    fn converged_round_trip() {
        round_trip(envelope_with(AgentEvent::Converged {
            converged_at: fixed_now(),
            current_closure: "closure-a".into(),
            seq: 11,
        }));
    }

    #[test]
    fn probe_status_wire_format_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&ProbeStatusWire::Pass).unwrap(),
            "\"pass\"",
        );
        assert_eq!(
            serde_json::to_string(&ProbeStatusWire::Fail).unwrap(),
            "\"fail\"",
        );
    }

    #[test]
    fn on_health_failure_wire_format_is_kebab_case() {
        assert_eq!(
            serde_json::to_string(&OnHealthFailureWire::Halt).unwrap(),
            "\"halt\"",
        );
        assert_eq!(
            serde_json::to_string(&OnHealthFailureWire::RollbackAndHalt).unwrap(),
            "\"rollback-and-halt\"",
        );
    }

    #[test]
    fn seq_accessor_matches_variant_field() {
        let e = AgentEvent::DispatchAck {
            current_closure_at_dispatch: "x".into(),
            received_at: fixed_now(),
            seq: 42,
        };
        assert_eq!(e.seq(), 42);
    }
}
