//! RFC-0012 §10 + §6.1 item 4 re-derivability invariant.
//!
//! Walking `event_log` chronologically must reproduce the `rollouts`
//! derived view from empty. This test stages a known rollout lifecycle
//! against an in-memory DB, then rebuilds a shadow `rollouts` table
//! from the event_log walk by replaying each `rollout_event` through
//! the pure reducer (`nixfleet_state_machine::rollout::step`) and
//! interpreting effects.
//!
//! Drift between the live applier-written table and the rebuild path
//! is the bug class this test prevents — exactly the bug class the
//! derived-view discipline (RFC-0011 §2.4) exists to eliminate.

use chrono::{DateTime, Duration, TimeZone, Utc};
use nixfleet_control_plane::db::Db;
use nixfleet_control_plane::db::event_log::EventLogKind;
use nixfleet_state_machine::HostState;
use nixfleet_state_machine::rollout::{
    self as rollout_sm, RolloutEffect, RolloutEvent, RolloutRecord, RolloutState,
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
}

/// Apply one `RolloutEffect` to the given DB — mirrors the applier's
/// `apply_rollout_effect`.
fn apply_effect(db: &Db, now: DateTime<Utc>, effect: RolloutEffect) {
    match effect {
        RolloutEffect::RecordRolloutTransition {
            rollout_id, to, at, ..
        } => {
            db.rollouts()
                .record_rollout_transition(rollout_id.as_str(), to, at, None)
                .unwrap();
        }
        RolloutEffect::UpdateCurrentWave { rollout_id, wave } => {
            db.rollouts()
                .set_current_wave(rollout_id.as_str(), wave, None)
                .unwrap();
        }
        RolloutEffect::InsertQuarantineFromRollout {
            channel,
            closure_hash,
        } => {
            db.quarantined_closures()
                .insert(&channel, &closure_hash, now, None)
                .unwrap();
        }
        RolloutEffect::SchedulePruning { .. } => {
            // Deferred per RFC-0012 §3 / §13; no-op in 10b.
        }
    }
}

/// Replay a `RolloutEvent` against the rollouts table by:
/// 1. Reading the current record (or constructing the initial one for
///    `RolloutOpened`).
/// 2. Stepping the reducer with the event.
/// 3. Applying each emitted effect.
fn replay_event(db: &Db, now: DateTime<Utc>, event: &RolloutEvent) {
    if let RolloutEvent::RolloutOpened {
        rollout_id,
        channel,
        target_ref,
        at,
    } = event
    {
        db.rollouts()
            .record_rollout_opened(rollout_id.as_str(), channel, target_ref, *at, None)
            .unwrap();
        return;
    }

    let rollout_id = match event {
        RolloutEvent::SuccessorOpened {
            superseded_rollout_id,
            ..
        } => superseded_rollout_id.clone(),
        RolloutEvent::HostJoined { rollout_id, .. }
        | RolloutEvent::HostStateChanged { rollout_id, .. }
        | RolloutEvent::WaveAdvanced { rollout_id, .. }
        | RolloutEvent::RolloutTerminal { rollout_id, .. }
        | RolloutEvent::RetentionExpired { rollout_id, .. }
        | RolloutEvent::OperatorClearance { rollout_id, .. } => rollout_id.clone(),
        RolloutEvent::RolloutOpened { .. } => unreachable!(),
    };

    let Some(row) = db.rollouts().state(rollout_id.as_str()).unwrap() else {
        return;
    };
    let record = RolloutRecord {
        rollout_id: row.rollout_id,
        channel: row.channel,
        target_ref: row.target_ref,
        state: row.state,
        current_wave: row.current_wave,
        opened_event_log_seq: row.opened_event_log_seq,
        last_transition_event_log_seq: row.last_transition_event_log_seq,
        opened_at: row.opened_at,
        terminal_at: row.terminal_at,
        superseded_at: row.superseded_at,
    };

    if let Ok((_record, effects)) = rollout_sm::step(record, event.clone(), now) {
        for effect in effects {
            apply_effect(db, now, effect);
        }
    }
}

/// Compare two `RolloutRow`s by every operator-visible field. Excludes
/// `event_log_seq` FKs — those are NULL under v0.2.1 baseline and
/// rebuild-path entries have different (newly-assigned) seqs in any
/// case.
fn assert_rows_equivalent(
    a: &nixfleet_control_plane::db::rollouts::RolloutRow,
    b: &nixfleet_control_plane::db::rollouts::RolloutRow,
) {
    assert_eq!(a.rollout_id, b.rollout_id, "rollout_id");
    assert_eq!(a.channel, b.channel, "channel");
    assert_eq!(a.target_ref, b.target_ref, "target_ref");
    assert_eq!(a.state, b.state, "state");
    assert_eq!(a.current_wave, b.current_wave, "current_wave");
    assert_eq!(a.opened_at, b.opened_at, "opened_at");
    assert_eq!(a.terminal_at, b.terminal_at, "terminal_at");
    assert_eq!(a.superseded_at, b.superseded_at, "superseded_at");
}

/// Apply an event to the LIVE db (mirrors the applier's path: write
/// the row via `record_rollout_*` AND append `event_log` entry).
///
/// The `RolloutOpened` event is the creation marker so we call
/// `record_rollout_opened` directly. All other events go through the
/// reducer + `apply_effect` so the test's "live" path matches the
/// applier's `process_rollout_event` flow.
fn drive_live(db: &Db, now: DateTime<Utc>, event: &RolloutEvent) {
    use nixfleet_control_plane::db::event_log::EventLogEntry;
    // 1) Mutate state.
    replay_event(db, now, event);
    // 2) Append event_log entry. Payload need only be valid JSON for
    // append() to accept it; the rederivability walk reads payload via
    // a hand-rolled dispatch below.
    db.event_log()
        .append(&EventLogEntry {
            kind: EventLogKind::RolloutEvent,
            ts: now,
            host_id: None,
            rollout_id: Some(event_rollout_id(event).as_str().to_string()),
            payload: serde_json::to_string(event).unwrap(),
        })
        .unwrap();
}

fn event_rollout_id(event: &RolloutEvent) -> &nixfleet_proto::RolloutId {
    match event {
        RolloutEvent::RolloutOpened { rollout_id, .. }
        | RolloutEvent::HostJoined { rollout_id, .. }
        | RolloutEvent::HostStateChanged { rollout_id, .. }
        | RolloutEvent::WaveAdvanced { rollout_id, .. }
        | RolloutEvent::RolloutTerminal { rollout_id, .. }
        | RolloutEvent::RetentionExpired { rollout_id, .. }
        | RolloutEvent::OperatorClearance { rollout_id, .. } => rollout_id,
        RolloutEvent::SuccessorOpened {
            superseded_rollout_id,
            ..
        } => superseded_rollout_id,
    }
}

#[test]
fn single_rollout_full_lifecycle_round_trips() {
    let live = Db::open_in_memory().unwrap();
    live.migrate().unwrap();

    let r1 = "r1";
    let channel = "stable";
    let target_ref = "ref-1";

    let events = vec![
        (
            t0(),
            RolloutEvent::RolloutOpened {
                rollout_id: r1.into(),
                channel: channel.into(),
                target_ref: target_ref.into(),
                at: t0(),
            },
        ),
        (
            t0() + Duration::seconds(1),
            RolloutEvent::HostJoined {
                rollout_id: r1.into(),
                host_id: "h1".into(),
                wave: 0,
                at: t0() + Duration::seconds(1),
            },
        ),
        (
            t0() + Duration::seconds(2),
            RolloutEvent::HostStateChanged {
                rollout_id: r1.into(),
                host_id: "h1".into(),
                from: HostState::Soaking,
                to: HostState::Converged,
                at: t0() + Duration::seconds(2),
            },
        ),
        (
            t0() + Duration::seconds(3),
            RolloutEvent::RolloutTerminal {
                rollout_id: r1.into(),
                at: t0() + Duration::seconds(3),
            },
        ),
    ];

    for (now, event) in &events {
        drive_live(&live, *now, event);
    }
    let live_row = live.rollouts().state(r1).unwrap().expect("rollout present");
    assert_eq!(live_row.state, RolloutState::Terminal);

    // Now rebuild from event_log walk into a fresh DB.
    let rebuilt = Db::open_in_memory().unwrap();
    rebuilt.migrate().unwrap();

    let rows = live
        .event_log()
        .query_by_kind(EventLogKind::RolloutEvent, 1000)
        .unwrap();
    for row in &rows {
        let event: RolloutEvent = serde_json::from_str(&row.payload).expect("payload deserializes");
        replay_event(&rebuilt, row.ts, &event);
    }

    let rebuilt_row = rebuilt
        .rollouts()
        .state(r1)
        .unwrap()
        .expect("rebuilt rollout present");
    assert_rows_equivalent(&live_row, &rebuilt_row);
}

#[test]
fn supersession_round_trips() {
    let live = Db::open_in_memory().unwrap();
    live.migrate().unwrap();

    let r1 = "r1";
    let r2 = "r2";
    let channel = "stable";

    let events = vec![
        (
            t0(),
            RolloutEvent::RolloutOpened {
                rollout_id: r1.into(),
                channel: channel.into(),
                target_ref: "ref-1".into(),
                at: t0(),
            },
        ),
        (
            t0() + Duration::seconds(1),
            RolloutEvent::HostJoined {
                rollout_id: r1.into(),
                host_id: "h1".into(),
                wave: 0,
                at: t0() + Duration::seconds(1),
            },
        ),
        (
            t0() + Duration::seconds(60),
            RolloutEvent::SuccessorOpened {
                superseded_rollout_id: r1.into(),
                successor_rollout_id: r2.into(),
                at: t0() + Duration::seconds(60),
            },
        ),
        (
            t0() + Duration::seconds(60),
            RolloutEvent::RolloutOpened {
                rollout_id: r2.into(),
                channel: channel.into(),
                target_ref: "ref-2".into(),
                at: t0() + Duration::seconds(60),
            },
        ),
    ];

    for (now, event) in &events {
        drive_live(&live, *now, event);
    }
    let live_r1 = live.rollouts().state(r1).unwrap().unwrap();
    let live_r2 = live.rollouts().state(r2).unwrap().unwrap();
    assert_eq!(live_r1.state, RolloutState::Superseded);
    assert_eq!(live_r2.state, RolloutState::Opening);

    let rebuilt = Db::open_in_memory().unwrap();
    rebuilt.migrate().unwrap();
    let rows = live
        .event_log()
        .query_by_kind(EventLogKind::RolloutEvent, 1000)
        .unwrap();
    for row in &rows {
        let event: RolloutEvent = serde_json::from_str(&row.payload).unwrap();
        replay_event(&rebuilt, row.ts, &event);
    }

    let rebuilt_r1 = rebuilt.rollouts().state(r1).unwrap().unwrap();
    let rebuilt_r2 = rebuilt.rollouts().state(r2).unwrap().unwrap();
    assert_rows_equivalent(&live_r1, &rebuilt_r1);
    assert_rows_equivalent(&live_r2, &rebuilt_r2);
}
