//! Reusable read-model substrate for fleet state. Consumed by the
//! `/v1/hosts` HTTP route and (forthcoming) Prometheus metrics exporter
//! + CLI status renderer. Sharing this means the row shape and label
//! set agree by construction across all three surfaces.

use std::collections::HashMap;

use nixfleet_proto::agent_wire::ReportEvent;
use nixfleet_proto::{HostRolloutState, HostStatusEntry};
use nixfleet_reconciler::compute_rollout_id_for_channel;
use nixfleet_reconciler::evidence::SignatureStatus;

use crate::server::AppState;

#[derive(Debug)]
pub enum StateViewError {
    /// Verified fleet snapshot not yet primed (CP just started; channel-refs
    /// poll hasn't completed a successful verify yet, or file-backed artifact
    /// failed verification).
    FleetNotPrimed,
}

/// Joins verified fleet declarations × per-host checkins × report buffers
/// into a one-row-per-declared-host view, sorted by hostname for stable
/// output. Outstanding-event counts apply resolution-by-replacement:
/// events from older rollouts than the host's `last_rollout_id` are
/// treated as resolved.
pub async fn fleet_state_view(state: &AppState) -> Result<Vec<HostStatusEntry>, StateViewError> {
    let snapshot = state
        .verified_fleet
        .read()
        .await
        .clone()
        .ok_or(StateViewError::FleetNotPrimed)?;
    let fleet = snapshot.fleet;
    let fleet_hash = snapshot.fleet_resolved_hash;
    let checkins = state.host_checkins.read().await;
    let reports = state.host_reports.read().await;

    // Memoise per-channel rollout ID so we project the manifest once per
    // channel, not per host. `None` covers both the err and ok-None cases —
    // either way, no current rollout for that channel.
    let mut current_rollout_for_channel: HashMap<String, Option<String>> = HashMap::new();
    for channel in fleet.channels.keys() {
        let id = compute_rollout_id_for_channel(&fleet, &fleet_hash, channel)
            .ok()
            .flatten();
        current_rollout_for_channel.insert(channel.clone(), id);
    }

    let mut entries: Vec<HostStatusEntry> = fleet
        .hosts
        .iter()
        .map(|(hostname, host_decl)| {
            let checkin = checkins.get(hostname);
            let last_checkin_at = checkin.map(|c| c.last_checkin);
            let current = checkin.map(|c| c.checkin.current_generation.closure_hash.clone());
            let pending = checkin.and_then(|c| {
                c.checkin
                    .pending_generation
                    .as_ref()
                    .map(|p| p.closure_hash.clone())
            });
            let last_rollout_id = checkin.and_then(|c| {
                c.checkin
                    .last_evaluated_target
                    .as_ref()
                    .map(|t| t.rollout_id.clone())
            });
            let converged = match (&host_decl.closure_hash, &current) {
                (Some(declared), Some(running)) => declared == running,
                _ => false,
            };

            let host_buf = reports.get(hostname);
            let cur_rollout = last_rollout_id.as_deref();
            let mut compliance_failures = 0usize;
            let mut runtime_gate_errors = 0usize;
            let mut verified_count = 0usize;
            if let Some(buf) = host_buf {
                for record in buf.iter() {
                    let is_compliance =
                        matches!(record.report.event, ReportEvent::ComplianceFailure { .. });
                    let is_runtime_gate =
                        matches!(record.report.event, ReportEvent::RuntimeGateError { .. });
                    if !is_compliance && !is_runtime_gate {
                        continue;
                    }
                    let event_rollout = record.report.rollout.as_deref();
                    let outstanding = !matches!(
                        (cur_rollout, event_rollout),
                        (Some(cur), Some(ev_r)) if cur != ev_r
                    );
                    if !outstanding {
                        continue;
                    }
                    if is_compliance {
                        compliance_failures += 1;
                    }
                    if is_runtime_gate {
                        runtime_gate_errors += 1;
                    }
                    if matches!(record.signature_status, Some(SignatureStatus::Verified)) {
                        verified_count += 1;
                    }
                }
            }

            let last_uptime_secs = checkin.and_then(|c| c.checkin.uptime_secs);

            // GOTCHA: query state for the FLEET's current rolloutId for this
            // channel, not the agent-reported last_rollout_id (may be stale
            // after a fresh deploy supersedes). Returns None when:
            //   - no DB configured (in-memory CP),
            //   - no current rollout (channel has no host with a closure),
            //   - DB row absent (host hasn't transitioned for this rollout yet),
            //   - DB row has an unrecognised state string (parse fail).
            let rollout_state = state.db.as_ref().and_then(|db| {
                let rid = current_rollout_for_channel
                    .get(&host_decl.channel)
                    .and_then(|o| o.as_deref())?;
                let s = db.rollout_state().host_state(hostname, rid).ok().flatten()?;
                HostRolloutState::from_db_str(&s).ok()
            });

            HostStatusEntry {
                hostname: hostname.clone(),
                channel: host_decl.channel.clone(),
                declared_closure_hash: host_decl.closure_hash.clone(),
                current_closure_hash: current,
                pending_closure_hash: pending,
                last_checkin_at,
                last_rollout_id,
                converged,
                outstanding_compliance_failures: compliance_failures,
                outstanding_runtime_gate_errors: runtime_gate_errors,
                verified_event_count: verified_count,
                last_uptime_secs,
                rollout_state,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.hostname.cmp(&b.hostname));
    Ok(entries)
}
