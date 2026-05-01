//! Cross-module test fixtures. `pub(crate)` so each `db/*.rs::tests`
//! can pull from one place without duplicating boilerplate.

use chrono::{DateTime, Utc};

use super::confirms::PendingConfirmInsert;
use super::reports::HostReportInsert;
use super::Db;
use crate::state::{HealthyMarker, HostRolloutState};

pub(crate) fn fresh_db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db
}

/// Shorthand for the legacy "record host as Healthy with marker
/// stamp" call, expressed via the new typed transition. Reduces
/// churn in the broader test corpus and keeps each assertion
/// focused on its scenario.
pub(crate) fn mark_healthy(db: &Db, host: &str, rollout: &str, now: DateTime<Utc>) {
    db.rollout_state()
        .transition_host_state(
            host,
            rollout,
            HostRolloutState::Healthy,
            HealthyMarker::Set(now),
            None,
        )
        .unwrap();
}

/// Build a `PendingConfirmInsert` with the common shape used across
/// the test module (rollout_id reused as channel_ref, mirroring how
/// `dispatch.rs` populates the row).
pub(crate) fn pc_insert<'a>(
    host: &'a str,
    rollout: &'a str,
    target_closure: &'a str,
    deadline: DateTime<Utc>,
) -> PendingConfirmInsert<'a> {
    PendingConfirmInsert {
        hostname: host,
        rollout_id: rollout,
        channel: "stable",
        wave: 0,
        target_closure_hash: target_closure,
        target_channel_ref: rollout,
        confirm_deadline: deadline,
    }
}

pub(crate) fn fail_event<'a>(
    rollout: Option<&'a str>,
    sig: Option<&'a str>,
) -> HostReportInsert<'a> {
    HostReportInsert {
        hostname: "lab",
        event_id: "evt-test",
        received_at: Utc::now(),
        event_kind: "compliance-failure",
        rollout,
        signature_status: sig,
        report_json: r#"{"hostname":"lab","agentVersion":"test"}"#,
    }
}
