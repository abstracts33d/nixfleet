//! Rollouts derived-view table (RFC-0008 §6.3). The applier is the sole
//! writer; every state-mutating method takes an
//! `event_log_seq: Option<i64>` so the row's `last_transition_event_log_seq`
//! FK can be populated.
//!
//! Phase 10a baseline: the rollout reducer (Phase 10b) is unimplemented
//! and the applier still drives transitions via the legacy PlanAction
//! path. The new method shape (event_log_seq arg, state enum, target_ref)
//! is ready; Phase 10b lights up the reducer that drives them through the
//! `RolloutEffect` interpretation in the applier.
//!
//! `event_log_seq` is NULL-able under the v0.2.1 baseline (RFC-0008 §6.1
//! item 3 + `.claude/plans/v0.2.1-followups.md` #1); same as
//! `probe_failures.event_log_seq` (RFC-0007 §7.2).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Mutex;

use nixfleet_proto::RolloutId;
use nixfleet_state_machine::rollout::RolloutState;

/// Raw tuple shape of a `rollouts` row, as read by rusqlite. Fields:
/// `(rollout_id, channel, target_ref, state, current_wave,
///   opened_event_log_seq, last_transition_event_log_seq, opened_at,
///   terminal_at, superseded_at)`. Aliased so clippy doesn't flag the
/// type complexity on the inline closure.
type RolloutRowTuple = (
    RolloutId,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
);

pub struct Rollouts<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

/// Typed projection of a single `rollouts` row. Replaces the v0.1-era
/// `SupersedeStatus` (which only carried `superseded_at`/`superseded_by`/
/// `terminal_at`) with the full row shape so callers can read `state`
/// directly without ad-hoc boolean derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutRow {
    pub rollout_id: RolloutId,
    pub channel: String,
    pub target_ref: String,
    pub state: RolloutState,
    pub current_wave: u32,
    pub opened_event_log_seq: Option<i64>,
    pub last_transition_event_log_seq: Option<i64>,
    pub opened_at: DateTime<Utc>,
    pub terminal_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
}

impl Rollouts<'_> {
    /// Pure-insert of a new `Opening`-state rollout row. Idempotent on
    /// `rollout_id` PK (INSERT OR IGNORE) — no side effects on other
    /// rows.
    ///
    /// Supersession of prior in-flight rollouts on the same channel is
    /// driven through the rollout reducer (Phase 10c): the applier
    /// snapshots in-flight predecessors, calls this method, then routes
    /// a `RolloutEvent::SuccessorOpened` per predecessor through
    /// `process_rollout_event`. The reducer transitions each predecessor
    /// from its current state to `Superseded` and emits a
    /// `RolloutEffect::RecordRolloutTransition` that the applier writes
    /// via `record_rollout_transition`. Closes the last RFC-0004 §3
    /// "implicit side effect" anti-pattern in Phase 10.
    pub fn record_rollout_opened(
        &self,
        rollout_id: &str,
        channel: &str,
        target_ref: &str,
        opened_at: DateTime<Utc>,
        opened_event_log_seq: Option<i64>,
    ) -> Result<()> {
        let opened_rfc = opened_at.to_rfc3339();
        super::read(self.conn, |c| {
            c.execute(
                "INSERT OR IGNORE INTO rollouts(
                     rollout_id, channel, target_ref, state, current_wave,
                     opened_event_log_seq, last_transition_event_log_seq,
                     opened_at)
                 VALUES (?1, ?2, ?3, 'Opening', 0, ?4, ?4, ?5)",
                params![
                    rollout_id,
                    channel,
                    target_ref,
                    opened_event_log_seq,
                    opened_rfc
                ],
            )
            .context("INSERT OR IGNORE rollouts")
            .map(|_| ())
        })
    }

    /// Record a state transition on an existing rollout row. Stamps the
    /// `state` column, the appropriate timestamp side-effect
    /// (`terminal_at` / `superseded_at`), and the
    /// `last_transition_event_log_seq` FK.
    ///
    /// Idempotent on `(rollout_id, target_state)`: if the row is already
    /// at `to`, the UPDATE no-ops via the `WHERE state != ?` guard.
    pub fn record_rollout_transition(
        &self,
        rollout_id: &str,
        to: RolloutState,
        at: DateTime<Utc>,
        event_log_seq: Option<i64>,
    ) -> Result<usize> {
        let at_rfc = at.to_rfc3339();
        let to_str = to.as_db_str();
        // SQLite has no enum; choose the timestamp side-effect by
        // matching on `to` in Rust and building the SQL accordingly.
        let (sql, bind_terminal, bind_superseded): (&str, bool, bool) = match to {
            RolloutState::Terminal => (
                "UPDATE rollouts
                 SET state = ?2,
                     last_transition_event_log_seq = ?3,
                     terminal_at = ?4
                 WHERE rollout_id = ?1 AND state != ?2",
                true,
                false,
            ),
            RolloutState::Superseded => (
                "UPDATE rollouts
                 SET state = ?2,
                     last_transition_event_log_seq = ?3,
                     superseded_at = ?4
                 WHERE rollout_id = ?1 AND state != ?2",
                false,
                true,
            ),
            _ => (
                "UPDATE rollouts
                 SET state = ?2,
                     last_transition_event_log_seq = ?3
                 WHERE rollout_id = ?1 AND state != ?2",
                false,
                false,
            ),
        };
        super::read(self.conn, |c| {
            if bind_terminal || bind_superseded {
                c.execute(sql, params![rollout_id, to_str, event_log_seq, at_rfc])
                    .context("UPDATE rollouts state (with timestamp side-effect)")
            } else {
                c.execute(sql, params![rollout_id, to_str, event_log_seq])
                    .context("UPDATE rollouts state")
            }
        })
    }

    /// Monotonic wave-index advance; `WHERE current_wave < ?2` blocks
    /// concurrent ticks from racing backwards. Stamps the
    /// `last_transition_event_log_seq` FK alongside.
    pub fn set_current_wave(
        &self,
        rollout_id: &str,
        wave: u32,
        event_log_seq: Option<i64>,
    ) -> Result<usize> {
        super::read(self.conn, |c| {
            c.execute(
                "UPDATE rollouts
                 SET current_wave = ?2,
                     last_transition_event_log_seq = COALESCE(?3, last_transition_event_log_seq)
                 WHERE rollout_id = ?1 AND current_wave < ?2",
                params![rollout_id, wave as i64, event_log_seq],
            )
            .context("set_current_wave")
        })
    }

    pub fn current_wave(&self, rollout_id: &str) -> Result<Option<u32>> {
        super::read(self.conn, |c| {
            c.query_row(
                "SELECT current_wave FROM rollouts WHERE rollout_id = ?1",
                params![rollout_id],
                |row| row.get::<_, i64>(0).map(|w| w as u32),
            )
            .optional()
            .context("query rollouts.current_wave")
        })
    }

    /// Full row projection, or `None` if the rollout is unknown. Callers
    /// project `state` directly; the v0.1 `is_superseded`/`is_terminal`/
    /// `is_finished` boolean derivations are gone (use `state ==
    /// RolloutState::X` reads).
    pub fn state(&self, rollout_id: &str) -> Result<Option<RolloutRow>> {
        super::read(self.conn, |c| {
            let row = c
                .query_row(
                    "SELECT rollout_id, channel, target_ref, state, current_wave,
                            opened_event_log_seq, last_transition_event_log_seq,
                            opened_at, terminal_at, superseded_at
                     FROM rollouts
                     WHERE rollout_id = ?1",
                    params![rollout_id],
                    |row| -> rusqlite::Result<RolloutRowTuple> {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    },
                )
                .optional()
                .context("query rollouts.state")?;
            row.map(|t| -> Result<RolloutRow> {
                let parse_ts =
                    |raw: Option<String>, field: &str| -> Result<Option<DateTime<Utc>>> {
                        match raw {
                            Some(s) => Ok(Some(
                                s.parse::<DateTime<Utc>>()
                                    .with_context(|| format!("parse rollouts.{field}: {s}"))?,
                            )),
                            None => Ok(None),
                        }
                    };
                let state = RolloutState::from_db_str(&t.3).ok_or_else(|| {
                    anyhow::anyhow!("unknown rollouts.state value: {} (CHECK violation?)", t.3)
                })?;
                Ok(RolloutRow {
                    rollout_id: t.0,
                    channel: t.1,
                    target_ref: t.2,
                    state,
                    current_wave: t.4 as u32,
                    opened_event_log_seq: t.5,
                    last_transition_event_log_seq: t.6,
                    opened_at: t
                        .7
                        .parse::<DateTime<Utc>>()
                        .with_context(|| format!("parse rollouts.opened_at: {}", t.7))?,
                    terminal_at: parse_ts(t.8, "terminal_at")?,
                    superseded_at: parse_ts(t.9, "superseded_at")?,
                })
            })
            .transpose()
        })
    }

    /// Gate-observed source. Filters `Superseded` and `Pruned` only —
    /// terminal rollouts stay visible so channel-edges can detect
    /// "predecessor converged". UI consumers should use `list_in_flight`.
    pub fn list_active(&self) -> Result<GateRollouts> {
        Ok(GateRollouts(self.list_filtered(false)?))
    }

    /// UI source. Filters `Superseded`, `Pruned`, AND `Terminal`
    /// (operator's "done" view).
    pub fn list_in_flight(&self) -> Result<UiRollouts> {
        Ok(UiRollouts(self.list_filtered(true)?))
    }

    fn list_filtered(&self, exclude_terminal: bool) -> Result<Vec<ActiveRollout>> {
        let sql = if exclude_terminal {
            "SELECT rollout_id, channel, current_wave, opened_at, terminal_at
             FROM rollouts
             WHERE state NOT IN ('Superseded', 'Pruned', 'Terminal')
             ORDER BY opened_at DESC, rollout_id"
        } else {
            "SELECT rollout_id, channel, current_wave, opened_at, terminal_at
             FROM rollouts
             WHERE state NOT IN ('Superseded', 'Pruned')
             ORDER BY opened_at DESC, rollout_id"
        };
        let rows: Vec<(ActiveRollout, Option<String>)> = super::read(self.conn, |c| {
            let mut stmt = c.prepare(sql)?;
            let v = stmt
                .query_map([], |row| {
                    let terminal_at_raw: Option<String> = row.get(4)?;
                    Ok((
                        ActiveRollout {
                            rollout_id: row.get(0)?,
                            channel: row.get(1)?,
                            current_wave: row.get::<_, i64>(2)? as u32,
                            created_at: row.get::<_, String>(3)?,
                            terminal_at: None,
                        },
                        terminal_at_raw,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(v)
        })?;
        rows.into_iter()
            .map(|(mut row, raw)| -> Result<ActiveRollout> {
                row.terminal_at = match raw {
                    Some(s) => Some(
                        s.parse::<DateTime<Utc>>()
                            .with_context(|| format!("parse rollouts.terminal_at: {s}"))?,
                    ),
                    None => None,
                };
                Ok(row)
            })
            .collect()
    }

    /// Prune finished (Superseded | Terminal | Failed | Reverted)
    /// rollouts past `max_age_hours` AND their `host_rollout_records`
    /// rows. Returns `(host_rollout_records_pruned, rollouts_pruned)`.
    ///
    /// Phase 10b: this physical-deletion pass becomes a
    /// `RetentionExpired` event emission instead, transitioning the row
    /// to `Pruned` (the row persists for audit; v0.3 retention-
    /// compaction handles physical deletion per RFC-0008 §3 + §13).
    /// For 10a we keep the physical prune so the existing operator
    /// workflow stays unchanged while the rollout reducer is
    /// unimplemented.
    pub fn prune_finished_rollouts(&self, max_age_hours: i64) -> Result<(usize, usize)> {
        let cutoff_str = (Utc::now() - chrono::Duration::hours(max_age_hours)).to_rfc3339();
        super::txn(self.conn, "prune_finished_rollouts", |t| {
            let records_pruned = t
                .execute(
                    "DELETE FROM host_rollout_records
                     WHERE rollout_id IN (
                         SELECT rollout_id FROM rollouts
                         WHERE state IN ('Superseded', 'Terminal', 'Failed', 'Reverted')
                           AND (
                               (superseded_at IS NOT NULL AND superseded_at < ?1)
                               OR (terminal_at IS NOT NULL AND terminal_at < ?1)
                           )
                     )",
                    params![&cutoff_str],
                )
                .context("DELETE host_rollout_records for finished rollouts")?;
            let rollouts_pruned = t
                .execute(
                    "DELETE FROM rollouts
                     WHERE state IN ('Superseded', 'Terminal', 'Failed', 'Reverted')
                       AND (
                           (superseded_at IS NOT NULL AND superseded_at < ?1)
                           OR (terminal_at IS NOT NULL AND terminal_at < ?1)
                       )",
                    params![&cutoff_str],
                )
                .context("DELETE rollouts (finished + past retention)")?;
            Ok((records_pruned, rollouts_pruned))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRollout {
    pub rollout_id: RolloutId,
    pub channel: String,
    pub current_wave: u32,
    pub created_at: String,
    /// Set on terminal transition; threaded into the in-memory `Rollout`
    /// so `advance_rollout` short-circuits and `channel_edges` can
    /// distinguish "predecessor converged" from "predecessor unknown".
    pub terminal_at: Option<DateTime<Utc>>,
}

/// Gate-observed view (keeps terminal). Type-disjoint from `UiRollouts`
/// so a wrong query result can't leak into a gate consumer.
#[derive(Debug, Clone, Default)]
pub struct GateRollouts(Vec<ActiveRollout>);

/// UI view (drops terminal). Drives `/v1/rollouts`, deferrals, metrics.
#[derive(Debug, Clone, Default)]
pub struct UiRollouts(Vec<ActiveRollout>);

macro_rules! rollout_view_api {
    ($t:ident) => {
        impl $t {
            pub fn iter(&self) -> std::slice::Iter<'_, ActiveRollout> {
                self.0.iter()
            }
            pub fn len(&self) -> usize {
                self.0.len()
            }
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
            pub fn into_inner(self) -> Vec<ActiveRollout> {
                self.0
            }
        }
        impl IntoIterator for $t {
            type Item = ActiveRollout;
            type IntoIter = std::vec::IntoIter<ActiveRollout>;
            fn into_iter(self) -> Self::IntoIter {
                self.0.into_iter()
            }
        }
        impl<'a> IntoIterator for &'a $t {
            type Item = &'a ActiveRollout;
            type IntoIter = std::slice::Iter<'a, ActiveRollout>;
            fn into_iter(self) -> Self::IntoIter {
                self.0.iter()
            }
        }
    };
}
rollout_view_api!(GateRollouts);
rollout_view_api!(UiRollouts);

impl GateRollouts {
    /// One-way demotion (UI is a strict subset; reverse direction is a
    /// type error so missing terminals can't be silently fabricated).
    pub fn into_ui(self) -> UiRollouts {
        UiRollouts(
            self.0
                .into_iter()
                .filter(|r| r.terminal_at.is_none())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn t0() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    #[test]
    fn record_rollout_opened_inserts_first_one_as_opening() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        let row = db.rollouts().state("r1").unwrap().expect("rollout present");
        assert_eq!(row.state, RolloutState::Opening);
        assert_eq!(row.target_ref, "ref-1");
        assert_eq!(row.channel, "stable");
        assert!(row.superseded_at.is_none());
    }

    /// Pure-insert assertion. Phase 10c deleted the inline supersession
    /// `UPDATE` from `record_rollout_opened`; the reducer-driven
    /// `SuccessorOpened` path now owns the predecessor → Superseded
    /// transition (re-derivability test covers it end-to-end). This test
    /// pins the new contract: opening a second rollout on the same
    /// channel leaves predecessors in their pre-existing state.
    #[test]
    fn record_rollout_opened_is_pure_insert() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        db.rollouts()
            .record_rollout_opened(
                "r2",
                "stable",
                "ref-2",
                t0() + chrono::Duration::seconds(1),
                None,
            )
            .unwrap();
        // r1 stays Opening — the applier (open_rollout) is responsible
        // for routing SuccessorOpened through process_rollout_event,
        // which drives the reducer transition. The DB method is now
        // pure-insert and does not side-effect on prior rows.
        assert_eq!(
            db.rollouts().state("r1").unwrap().unwrap().state,
            RolloutState::Opening
        );
        assert_eq!(
            db.rollouts().state("r2").unwrap().unwrap().state,
            RolloutState::Opening
        );
    }

    #[test]
    fn record_rollout_opened_does_not_supersede_across_channels() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        db.rollouts()
            .record_rollout_opened("r2", "edge-slow", "ref-2", t0(), None)
            .unwrap();
        assert_eq!(
            db.rollouts().state("r1").unwrap().unwrap().state,
            RolloutState::Opening
        );
        assert_eq!(
            db.rollouts().state("r2").unwrap().unwrap().state,
            RolloutState::Opening
        );
    }

    #[test]
    fn state_returns_none_for_unknown_rollout() {
        let db = fresh_db();
        assert!(db.rollouts().state("ghost").unwrap().is_none());
    }

    #[test]
    fn record_rollout_transition_stamps_terminal() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        let n = db
            .rollouts()
            .record_rollout_transition("r1", RolloutState::Terminal, t0(), None)
            .unwrap();
        assert_eq!(n, 1);
        let row = db.rollouts().state("r1").unwrap().unwrap();
        assert_eq!(row.state, RolloutState::Terminal);
        assert!(row.terminal_at.is_some());
        // Idempotent re-call no-ops.
        let n2 = db
            .rollouts()
            .record_rollout_transition("r1", RolloutState::Terminal, t0(), None)
            .unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn set_current_wave_is_monotonic_no_op_on_backwards() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        assert_eq!(db.rollouts().current_wave("r1").unwrap(), Some(0));
        let n = db.rollouts().set_current_wave("r1", 1, None).unwrap();
        assert_eq!(n, 1);
        assert_eq!(db.rollouts().current_wave("r1").unwrap(), Some(1));
        // Backwards is no-op.
        let n = db.rollouts().set_current_wave("r1", 0, None).unwrap();
        assert_eq!(n, 0);
        assert_eq!(db.rollouts().current_wave("r1").unwrap(), Some(1));
    }

    /// **Regression guard**: terminal rollouts STAY visible in
    /// `list_active` (the gate-observed source) but are HIDDEN from
    /// `list_in_flight` (the UI source). Same row, different views —
    /// this is the load-bearing semantic the v0.1 lifecycle attempts
    /// kept getting wrong.
    #[test]
    fn terminal_stays_in_list_active_but_drops_from_list_in_flight() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        db.rollouts()
            .record_rollout_opened("r2", "edge", "ref-2", t0(), None)
            .unwrap();
        assert_eq!(db.rollouts().list_active().unwrap().len(), 2);
        assert_eq!(db.rollouts().list_in_flight().unwrap().len(), 2);

        db.rollouts()
            .record_rollout_transition("r1", RolloutState::Terminal, t0(), None)
            .unwrap();

        let active = db.rollouts().list_active().unwrap();
        assert_eq!(
            active.len(),
            2,
            "list_active must include terminal rollouts so gates can see converged predecessors"
        );
        let r1_active = active
            .iter()
            .find(|r| r.rollout_id.as_str() == "r1")
            .unwrap();
        assert!(r1_active.terminal_at.is_some());

        let in_flight = db.rollouts().list_in_flight().unwrap().into_inner();
        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight[0].rollout_id.as_str(), "r2");
    }

    /// Superseded rollouts are dropped from BOTH views — supersession is
    /// the stronger signal (newer rollout for the same channel exists,
    /// gates evaluate against it).
    #[test]
    fn superseded_dropped_from_both_list_active_and_list_in_flight() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        // Phase 10c: supersession is reducer-driven via the applier; in
        // a db-level unit test we synthesize the end-state directly via
        // record_rollout_transition. Re-derivability through the reducer
        // is exercised in `tests/rollout_rederivability.rs`.
        db.rollouts()
            .record_rollout_transition(
                "r1",
                RolloutState::Superseded,
                t0() + chrono::Duration::seconds(1),
                None,
            )
            .unwrap();
        for rid in db.rollouts().list_active().unwrap().iter() {
            assert_ne!(rid.rollout_id.as_str(), "r1");
        }
        for rid in db.rollouts().list_in_flight().unwrap().iter() {
            assert_ne!(rid.rollout_id.as_str(), "r1");
        }
    }

    /// `GateRollouts.into_ui()` filters out terminal rollouts.
    #[test]
    fn gate_rollouts_into_ui_filters_terminal() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r-active", "stable", "ref-a", t0(), None)
            .unwrap();
        db.rollouts()
            .record_rollout_opened("r-converged", "edge", "ref-c", t0(), None)
            .unwrap();
        db.rollouts()
            .record_rollout_transition("r-converged", RolloutState::Terminal, t0(), None)
            .unwrap();
        let gate = db.rollouts().list_active().unwrap();
        assert_eq!(gate.len(), 2);
        let ui = gate.into_ui();
        assert_eq!(ui.len(), 1);
        assert_eq!(ui.into_inner()[0].rollout_id.as_str(), "r-active");
    }

    /// **Documentation test** — GateRollouts and UiRollouts must remain
    /// distinct types so a future commit can't conflate them. If a `From<
    /// UiRollouts> for GateRollouts` impl is added, the asymmetric
    /// `into_ui` invariant breaks; keep this test as a tripwire.
    #[test]
    fn gate_and_ui_rollouts_are_distinct_types() {
        let db = fresh_db();
        db.rollouts()
            .record_rollout_opened("r1", "stable", "ref-1", t0(), None)
            .unwrap();
        let _gate: super::GateRollouts = db.rollouts().list_active().unwrap();
        let _ui: super::UiRollouts = db.rollouts().list_in_flight().unwrap();
    }

    /// **Regression guard**: prune drops finished rollouts past
    /// retention AND their host_rollout_records rows; leaves in-flight
    /// rollouts and recent finishes alone.
    #[test]
    fn prune_finished_rollouts_drops_old_finished_keeps_recent_and_in_flight() {
        let db = fresh_db();
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::days(120);
        let recent = now - chrono::Duration::days(30);

        // r-active: in-flight, never touched. Must survive prune.
        db.rollouts()
            .record_rollout_opened("r-active", "stable", "ref-a", now, None)
            .unwrap();

        // r-old-superseded: superseded long ago. Phase 10c made
        // record_rollout_opened pure-insert; the prune scenario
        // drives the supersession transition explicitly via
        // record_rollout_transition (matches what the applier's
        // reducer-driven SuccessorOpened path would do).
        db.rollouts()
            .record_rollout_opened("r-old-superseded", "edge", "ref-os", now, None)
            .unwrap();
        db.rollouts()
            .record_rollout_opened("r-old-superseder", "edge", "ref-osr", now, None)
            .unwrap();
        db.rollouts()
            .record_rollout_transition("r-old-superseded", RolloutState::Superseded, old, None)
            .unwrap();

        // r-recent-terminal: terminal recently (30d). Should NOT prune.
        db.rollouts()
            .record_rollout_opened("r-recent-terminal", "preview", "ref-rt", now, None)
            .unwrap();
        db.rollouts()
            .record_rollout_transition("r-recent-terminal", RolloutState::Terminal, recent, None)
            .unwrap();

        // r-old-terminal: terminal long ago (120d). Should prune.
        db.rollouts()
            .record_rollout_opened("r-old-terminal", "preview-old", "ref-ot", now, None)
            .unwrap();
        db.rollouts()
            .record_rollout_transition("r-old-terminal", RolloutState::Terminal, old, None)
            .unwrap();

        // host_rollout_records rows tied to each.
        for rid in [
            "r-active",
            "r-old-superseded",
            "r-recent-terminal",
            "r-old-terminal",
        ] {
            let row = nixfleet_state_machine::HostRolloutState::new_pending(
                rid.into(),
                "host-x".to_string(),
                "stable".to_string(),
                format!("closure-{rid}"),
                now,
                now + chrono::Duration::minutes(5),
            );
            db.host_rollout_records().upsert(&row).unwrap();
        }

        let (records_pruned, rollouts_pruned) =
            db.rollouts().prune_finished_rollouts(24 * 90).unwrap();
        assert_eq!(rollouts_pruned, 2, "r-old-superseded + r-old-terminal");
        assert_eq!(records_pruned, 2);

        // r-active and r-recent-terminal retained.
        let active = db.rollouts().list_active().unwrap();
        let kept: Vec<&str> = active.iter().map(|r| r.rollout_id.as_str()).collect();
        assert!(kept.contains(&"r-active"));
        assert!(db.rollouts().state("r-recent-terminal").unwrap().is_some());
        assert!(db.rollouts().state("r-old-superseded").unwrap().is_none());
        assert!(db.rollouts().state("r-old-terminal").unwrap().is_none());
    }
}
