//! Prometheus metrics surface. Recorder is process-global; one
//! `PrometheusHandle` per process renders the text format on demand.
//!
//! Cardinality discipline (load-bearing): label sets carry only
//! values bounded by the verified fleet snapshot or the closed compliance
//! control set. Never label by `closure_hash`, `rollout_id`, or
//! `evidence_snippet` — those grow without bound and would blow up the
//! TSDB. Hostnames + channels + control IDs are the only safe labels.
//!
//! Scrape contract: `/metrics` calls `record_fleet_metrics` first to
//! refresh gauges from in-memory state, then renders. Counters
//! (compliance_failure_events_total) increment on event arrival in
//! `/v1/agent/report`, not on scrape.
//!
//! Init pattern: `install_recorder()` is idempotent via OnceLock. First
//! call installs the global; subsequent calls return the same handle.
//! Tests can therefore spin multiple test servers without colliding.

use std::sync::OnceLock;

use chrono::Utc;
use metrics::{counter, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use nixfleet_proto::HostStatusEntry;

use crate::server::AppState;
use crate::state_view::{fleet_state_view, StateViewError};

static METRICS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the process-global Prometheus recorder. Idempotent — safe to
/// call from each test's server-spawn helper.
pub fn install_recorder() -> &'static PrometheusHandle {
    METRICS_HANDLE.get_or_init(|| {
        PrometheusBuilder::new()
            .install_recorder()
            .expect("install Prometheus recorder")
    })
}

/// Counter increment hook for `/v1/agent/report` to call when a
/// `ComplianceFailure` event arrives. `control_id` is bounded by the
/// closed compliance crate's control set (currently 16).
pub fn record_compliance_event(control_id: &str) {
    counter!(
        "nixfleet_compliance_failure_events_total",
        "control_id" => control_id.to_string(),
    )
    .increment(1);
}

/// Counter increment hook for `RuntimeGateError` events.
pub fn record_runtime_gate_error() {
    counter!("nixfleet_runtime_gate_error_events_total").increment(1);
}

/// Increment when `gates::evaluate_for_host` returns `Some(GateBlock)`
/// at the dispatch endpoint. `gate_kind` is the kebab-case discriminator
/// (channel-edges / wave-promotion / host-edge / disruption-budget /
/// compliance-wave) — bounded set, safe label. Operators alert on
/// `rate(nixfleet_gate_block_total{gate="compliance-wave"}[5m]) > 0` or
/// similar to surface "enforce mode is actively holding hosts".
pub fn record_gate_block(gate_kind: &str) {
    counter!(
        "nixfleet_gate_block_total",
        "gate" => gate_kind.to_string(),
    )
    .increment(1);
}

/// Refresh per-host + per-channel gauges from the current fleet state
/// view. Called by the `/metrics` handler on every scrape — cheap (no
/// SQLite query, just RwLock reads + arithmetic).
pub async fn record_fleet_metrics(state: &AppState) -> Result<(), StateViewError> {
    let views = fleet_state_view(state).await?;
    let snapshot = state
        .verified_fleet
        .read()
        .await
        .clone()
        .ok_or(StateViewError::FleetNotPrimed)?;

    let now = Utc::now();
    for view in &views {
        record_host_gauges(view, now);
    }

    for (name, channel) in &snapshot.fleet.channels {
        gauge!(
            "nixfleet_channel_freshness_window_minutes",
            "channel" => name.clone(),
        )
        .set(f64::from(channel.freshness_window));
    }
    if let Some(signed_at) = snapshot.fleet.meta.signed_at {
        let age = now.signed_duration_since(signed_at).num_seconds().max(0);
        gauge!("nixfleet_fleet_signed_age_seconds").set(age as f64);
    }

    Ok(())
}

fn record_host_gauges(view: &HostStatusEntry, now: chrono::DateTime<Utc>) {
    let labels = [
        ("host", view.hostname.clone()),
        ("channel", view.channel.clone()),
    ];
    gauge!("nixfleet_host_converged", &labels[..]).set(if view.converged { 1.0 } else { 0.0 });
    gauge!(
        "nixfleet_host_outstanding_compliance_failures",
        &labels[..]
    )
    .set(view.outstanding_compliance_failures as f64);
    gauge!("nixfleet_host_outstanding_runtime_gate_errors", &labels[..])
        .set(view.outstanding_runtime_gate_errors as f64);
    gauge!("nixfleet_host_verified_event_count", &labels[..])
        .set(view.verified_event_count as f64);

    if let Some(last) = view.last_checkin_at {
        let age = now.signed_duration_since(last).num_seconds().max(0);
        gauge!(
            "nixfleet_host_last_checkin_seconds",
            "host" => view.hostname.clone(),
        )
        .set(age as f64);
    }

    if let Some(uptime) = view.last_uptime_secs {
        gauge!(
            "nixfleet_host_uptime_seconds",
            "host" => view.hostname.clone(),
        )
        .set(uptime as f64);
    }
}

/// Set once at server boot. `cp_build_info{version,git_commit}=1` is
/// the standard Prometheus pattern for tracking running version across
/// scrapes — operators alert on `changes(nixfleet_cp_build_info[1h])`.
pub fn record_build_info(version: &str, git_commit: Option<&str>) {
    gauge!(
        "nixfleet_cp_build_info",
        "version" => version.to_string(),
        "git_commit" => git_commit.unwrap_or("unknown").to_string(),
    )
    .set(1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_recorder_is_idempotent() {
        let h1 = install_recorder();
        let h2 = install_recorder();
        // Same OnceLock cell — pointer equality.
        assert!(std::ptr::eq(h1, h2), "recorder must be process-global");
    }

    #[test]
    fn rendered_output_contains_known_metric_after_increment() {
        let handle = install_recorder();
        record_compliance_event("ANSSI-BP-028");
        let body = handle.render();
        assert!(
            body.contains("nixfleet_compliance_failure_events_total"),
            "missing counter in render output:\n{body}"
        );
        assert!(
            body.contains("ANSSI-BP-028"),
            "missing control_id label:\n{body}"
        );
    }

    #[test]
    fn build_info_renders_with_labels() {
        let handle = install_recorder();
        record_build_info("0.2.0-test", Some("abc1234"));
        let body = handle.render();
        assert!(
            body.contains("nixfleet_cp_build_info"),
            "missing build_info gauge:\n{body}"
        );
        assert!(
            body.contains("version=\"0.2.0-test\""),
            "missing version label:\n{body}"
        );
    }

    #[test]
    fn gate_block_counter_renders_with_kebab_label() {
        let handle = install_recorder();
        record_gate_block("compliance-wave");
        record_gate_block("disruption-budget");
        let body = handle.render();
        assert!(
            body.contains("nixfleet_gate_block_total"),
            "missing gate_block counter:\n{body}"
        );
        assert!(
            body.contains("gate=\"compliance-wave\""),
            "missing compliance-wave label:\n{body}"
        );
        assert!(
            body.contains("gate=\"disruption-budget\""),
            "missing disruption-budget label:\n{body}"
        );
    }
}
