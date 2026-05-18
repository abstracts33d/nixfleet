//! Bidirectional conversions between the wire-format types in
//! `nixfleet-proto::agent_event` and the state-machine's effect / event
//! types. Both directions live in this crate (the state-machine) by the
//! orphan rule: every conversion has a state-machine-local type on at
//! least one side. Keeps `nixfleet-proto` free of state-machine
//! awareness (proto is the leaf crate) and CP free of duplicate wire
//! definitions (the architect's d013 lift per RFC-0004 §2).

use nixfleet_proto::agent_event::{
    AgentEvent, ProbeModeWire, ProbeStatusWire, ProbeSubResultWire, ProbeTopologyEntryWire,
};

use crate::effect::OutboundAgentEvent;
use crate::event::{Event, ProbeTopologyEntry};
use crate::state::{ProbeMode, ProbeStatus, ProbeSubResult};

// ─────────────────────────────────────────────────────────────────────
// Wire enum -> state-machine enum (inbound: CP receives an AgentEvent,
// projects it onto a Remote* reducer event).
// ─────────────────────────────────────────────────────────────────────

impl From<ProbeStatusWire> for ProbeStatus {
    fn from(w: ProbeStatusWire) -> Self {
        match w {
            ProbeStatusWire::Pass => ProbeStatus::Pass,
            ProbeStatusWire::Fail => ProbeStatus::Fail,
        }
    }
}

impl From<ProbeStatus> for ProbeStatusWire {
    fn from(s: ProbeStatus) -> Self {
        match s {
            ProbeStatus::Pass => ProbeStatusWire::Pass,
            ProbeStatus::Fail => ProbeStatusWire::Fail,
        }
    }
}

impl From<ProbeModeWire> for ProbeMode {
    fn from(w: ProbeModeWire) -> Self {
        match w {
            ProbeModeWire::Enforce => ProbeMode::Enforce,
            ProbeModeWire::Observe => ProbeMode::Observe,
            ProbeModeWire::Disabled => ProbeMode::Disabled,
        }
    }
}

impl From<ProbeMode> for ProbeModeWire {
    fn from(m: ProbeMode) -> Self {
        match m {
            ProbeMode::Enforce => ProbeModeWire::Enforce,
            ProbeMode::Observe => ProbeModeWire::Observe,
            ProbeMode::Disabled => ProbeModeWire::Disabled,
        }
    }
}

impl From<ProbeTopologyEntryWire> for ProbeTopologyEntry {
    fn from(w: ProbeTopologyEntryWire) -> Self {
        ProbeTopologyEntry {
            probe_name: w.probe_name,
            kind: w.kind,
            mode: w.mode.into(),
        }
    }
}

impl From<ProbeTopologyEntry> for ProbeTopologyEntryWire {
    fn from(e: ProbeTopologyEntry) -> Self {
        ProbeTopologyEntryWire {
            probe_name: e.probe_name,
            kind: e.kind,
            mode: e.mode.into(),
        }
    }
}

impl From<ProbeSubResultWire> for ProbeSubResult {
    fn from(w: ProbeSubResultWire) -> Self {
        ProbeSubResult {
            control_id: w.control_id,
            status: w.status.into(),
            framework: w.framework,
            article: w.article,
            effective_mode: w.effective_mode.into(),
            override_reason: w.override_reason,
        }
    }
}

impl From<ProbeSubResult> for ProbeSubResultWire {
    fn from(s: ProbeSubResult) -> Self {
        ProbeSubResultWire {
            control_id: s.control_id,
            status: s.status.into(),
            framework: s.framework,
            article: s.article,
            effective_mode: s.effective_mode.into(),
            override_reason: s.override_reason,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Inbound: CP receives wire AgentEvent, projects to a Remote* reducer
// Event. Replaces the local `AgentEvent::into_remote_event` impl that
// previously lived in CP's `routes/events.rs`.
// ─────────────────────────────────────────────────────────────────────

impl From<AgentEvent> for Event {
    fn from(e: AgentEvent) -> Self {
        match e {
            AgentEvent::DispatchAck {
                current_closure_at_dispatch,
                received_at,
                seq,
            } => Event::RemoteDispatchAck {
                current_closure_at_dispatch,
                received_at,
                seq,
            },
            AgentEvent::ActivationStarted {
                started_at,
                switch_method,
                seq,
            } => Event::RemoteActivationStarted {
                started_at,
                switch_method,
                seq,
            },
            AgentEvent::ActivationCompleted {
                observed_current_closure,
                exit_code,
                completed_at,
                seq,
            } => Event::RemoteActivationCompleted {
                observed_current_closure,
                exit_code,
                completed_at,
                seq,
            },
            AgentEvent::ActivationFailed {
                exit_code,
                stderr_tail,
                failed_at,
                seq,
            } => Event::RemoteActivationFailed {
                exit_code,
                stderr_tail,
                failed_at,
                seq,
            },
            AgentEvent::ActivationDeferred {
                component,
                deferred_at,
                seq,
            } => Event::RemoteActivationDeferred {
                component,
                deferred_at,
                seq,
            },
            AgentEvent::ProbeTopologyDeclared {
                probes,
                declared_at,
                seq,
            } => Event::RemoteProbeTopologyDeclared {
                probes: probes.into_iter().map(Into::into).collect(),
                declared_at,
                seq,
            },
            AgentEvent::ProbeObservedFirst {
                probe_name,
                mode,
                observed_at,
                seq,
            } => Event::RemoteProbeObservedFirst {
                probe_name,
                mode: mode.into(),
                observed_at,
                seq,
            },
            AgentEvent::ProbeResult {
                probe_name,
                mode,
                status,
                observed_at,
                failure_reason,
                sub_results,
                seq,
            } => Event::RemoteProbeResult {
                probe_name,
                mode: mode.into(),
                status: status.into(),
                observed_at,
                failure_reason,
                sub_results: sub_results
                    .map(|v| v.into_iter().map(Into::into).collect()),
                seq,
            },
            AgentEvent::ProbeFailureFirst {
                probe_name,
                mode,
                first_failed_at,
                seq,
            } => Event::RemoteProbeFailureFirst {
                probe_name,
                mode: mode.into(),
                first_failed_at,
                seq,
            },
            AgentEvent::Failed {
                failed_at,
                sustained_duration_secs,
                failing_probes,
                policy_applied,
                seq,
            } => Event::RemoteFailed {
                failed_at,
                sustained_duration_secs,
                failing_probes,
                policy_applied: policy_applied.into(),
                seq,
            },
            AgentEvent::RollbackComplete {
                reverted_to_closure,
                exit_code,
                completed_at,
                seq,
            } => Event::RemoteRollbackComplete {
                reverted_to_closure,
                exit_code,
                completed_at,
                seq,
            },
            AgentEvent::Converged {
                converged_at,
                current_closure,
                seq,
            } => Event::RemoteConverged {
                converged_at,
                current_closure,
                seq,
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Outbound: agent applier converts a state-machine OutboundAgentEvent
// into the wire AgentEvent before queuing it. Replaces the agent's
// hand-built `outbound_event_to_json` (which produced serde_json::Value
// of the same shape but with no compile-time typing).
// ─────────────────────────────────────────────────────────────────────

impl From<OutboundAgentEvent> for AgentEvent {
    fn from(e: OutboundAgentEvent) -> Self {
        match e {
            OutboundAgentEvent::DispatchAck {
                current_closure_at_dispatch,
                received_at,
                seq,
            } => AgentEvent::DispatchAck {
                current_closure_at_dispatch,
                received_at,
                seq,
            },
            OutboundAgentEvent::ActivationStarted {
                started_at,
                switch_method,
                seq,
            } => AgentEvent::ActivationStarted {
                started_at,
                switch_method,
                seq,
            },
            OutboundAgentEvent::ActivationCompleted {
                observed_current_closure,
                exit_code,
                completed_at,
                seq,
            } => AgentEvent::ActivationCompleted {
                observed_current_closure,
                exit_code,
                completed_at,
                seq,
            },
            OutboundAgentEvent::ActivationFailed {
                exit_code,
                stderr_tail,
                failed_at,
                seq,
            } => AgentEvent::ActivationFailed {
                exit_code,
                stderr_tail,
                failed_at,
                seq,
            },
            OutboundAgentEvent::ActivationDeferred {
                component,
                deferred_at,
                seq,
            } => AgentEvent::ActivationDeferred {
                component,
                deferred_at,
                seq,
            },
            OutboundAgentEvent::ProbeTopologyDeclared {
                probes,
                declared_at,
                seq,
            } => AgentEvent::ProbeTopologyDeclared {
                probes: probes.into_iter().map(Into::into).collect(),
                declared_at,
                seq,
            },
            OutboundAgentEvent::ProbeObservedFirst {
                probe_name,
                mode,
                observed_at,
                seq,
            } => AgentEvent::ProbeObservedFirst {
                probe_name,
                mode: mode.into(),
                observed_at,
                seq,
            },
            OutboundAgentEvent::ProbeResult {
                probe_name,
                mode,
                status,
                observed_at,
                failure_reason,
                sub_results,
                seq,
            } => AgentEvent::ProbeResult {
                probe_name,
                mode: mode.into(),
                status: status.into(),
                observed_at,
                failure_reason,
                sub_results: sub_results.map(|v| v.into_iter().map(Into::into).collect()),
                seq,
            },
            OutboundAgentEvent::ProbeFailureFirst {
                probe_name,
                mode,
                first_failed_at,
                seq,
            } => AgentEvent::ProbeFailureFirst {
                probe_name,
                mode: mode.into(),
                first_failed_at,
                seq,
            },
            OutboundAgentEvent::Failed {
                failed_at,
                sustained_duration_secs,
                failing_probes,
                policy_applied,
                seq,
            } => AgentEvent::Failed {
                failed_at,
                sustained_duration_secs,
                failing_probes,
                policy_applied: policy_applied.into(),
                seq,
            },
            OutboundAgentEvent::RollbackComplete {
                reverted_to_closure,
                exit_code,
                completed_at,
                seq,
            } => AgentEvent::RollbackComplete {
                reverted_to_closure,
                exit_code,
                completed_at,
                seq,
            },
            OutboundAgentEvent::Converged {
                converged_at,
                current_closure,
                seq,
            } => AgentEvent::Converged {
                converged_at,
                current_closure,
                seq,
            },
        }
    }
}
