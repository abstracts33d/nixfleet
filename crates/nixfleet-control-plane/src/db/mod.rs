//! SQLite persistence (rusqlite + refinery, WAL + FK).
//!
//! A single `Mutex<Connection>` is sufficient for fleet sizes ≤ 150
//! hosts (ADR-012); the migration trigger and pool target are
//! documented there. Schema lives under `migrations/`; `migrate()` is
//! idempotent + version-tracked. Mutex poisoning surfaces as anyhow
//! errors.
//!
//! Per-table operations live in submodules and are reached via
//! accessors on [`Db`]: `tokens()`, `host_dispatch_state()`,
//! `dispatch_history()`, `rollout_state()`, `reports()`,
//! `revocations()`. Each submodule's file header names the recovery
//! class (soft vs hard) per ARCHITECTURE.md §6 Phase 10.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub mod dispatch_history;
pub mod host_dispatch_state;
pub mod reports;
pub mod revocations;
pub mod rollout_state;
pub mod tokens;

pub use dispatch_history::DispatchHistoryRow;
pub use host_dispatch_state::{
    DispatchInsert, ExpiredDispatch, HostDispatchStateRow, RolloutDbSnapshot,
};
pub use reports::{HostReportInsert, HostReportRow};
pub use tokens::RecordTokenOutcome;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

/// SQLite-backed CP persistence.
pub struct Db {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("conn", &"<sqlite>").finish()
    }
}

impl Db {
    /// Open (or create) the SQLite database at `path`. Creates parent
    /// directories as needed. Enables WAL + FK on the connection
    /// before any migrations run.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir {}", parent.display()))?;
            }
        }
        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;

        // WAL improves concurrent read performance; FK enforces
        // referential integrity that the schema declares.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("set sqlite pragmas")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open a fresh in-memory database. Used by tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open sqlite :memory:")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        lock_conn(&self.conn)
    }

    /// Run all pending migrations. Idempotent under refinery —
    /// previously-applied migrations are skipped.
    pub fn migrate(&self) -> Result<()> {
        let mut guard = self.conn()?;
        embedded::migrations::runner()
            .run(&mut *guard)
            .context("run sqlite migrations")?;
        Ok(())
    }

    /// `token_replay` accessor (soft state).
    pub fn tokens(&self) -> tokens::Tokens<'_> {
        tokens::Tokens { conn: &self.conn }
    }

    /// `host_dispatch_state` accessor (soft state). One operational
    /// row per host; the joined `active_rollouts_snapshot`
    /// projection lives here too.
    pub fn host_dispatch_state(&self) -> host_dispatch_state::HostDispatchState<'_> {
        host_dispatch_state::HostDispatchState { conn: &self.conn }
    }

    /// `dispatch_history` accessor (soft state). Append-only audit
    /// log paired with `host_dispatch_state`; retention-pruned.
    pub fn dispatch_history(&self) -> dispatch_history::DispatchHistory<'_> {
        dispatch_history::DispatchHistory { conn: &self.conn }
    }

    /// `host_rollout_state` accessor (soft state).
    pub fn rollout_state(&self) -> rollout_state::RolloutState<'_> {
        rollout_state::RolloutState { conn: &self.conn }
    }

    /// `host_reports` accessor (soft state).
    pub fn reports(&self) -> reports::Reports<'_> {
        reports::Reports { conn: &self.conn }
    }

    /// `cert_revocations` accessor (hard state — see ARCHITECTURE.md
    /// §6 Phase 10).
    pub fn revocations(&self) -> revocations::Revocations<'_> {
        revocations::Revocations { conn: &self.conn }
    }
}

/// Lock the shared connection mutex, surfacing poisoning as an
/// `anyhow` error rather than a panic.
pub(crate) fn lock_conn(mu: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>> {
    mu.lock()
        .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
}

#[cfg(test)]
pub(crate) mod test_helpers;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_expected_tables() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let conn = db.conn().unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            names.contains(&"token_replay".to_string()),
            "tables: {names:?}"
        );
        assert!(names.contains(&"cert_revocations".to_string()));
        assert!(names.contains(&"host_dispatch_state".to_string()));
        assert!(names.contains(&"dispatch_history".to_string()));
        assert!(names.contains(&"host_rollout_state".to_string()));
        assert!(
            !names.contains(&"pending_confirms".to_string()),
            "V006 must drop the legacy table",
        );
    }

    #[test]
    fn v006_migrates_two_hosts_six_dispatches_correctly() {
        // Migration unit test (#81). Constructs the post-V005
        // pending_confirms shape inline (only the columns V006's
        // SELECT reads — the surrounding schema isn't relevant),
        // seeds it with two hosts × three dispatches each, runs
        // V006 by inclusion, then asserts the resulting tables
        // satisfy the invariants:
        //
        // - dispatch_history has every legacy row.
        // - host_dispatch_state has one row per hostname (the most
        //   recent legacy row by dispatched_at, id DESC).
        // - terminal_state is populated only on the rolled-back /
        //   cancelled legacy rows.
        // - pending_confirms is dropped.
        use rusqlite::params;
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            "CREATE TABLE pending_confirms (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 hostname TEXT NOT NULL,
                 rollout_id TEXT NOT NULL,
                 wave INTEGER NOT NULL,
                 target_closure_hash TEXT NOT NULL,
                 target_channel_ref TEXT NOT NULL,
                 dispatched_at TEXT NOT NULL DEFAULT (datetime('now')),
                 confirm_deadline TEXT NOT NULL,
                 confirmed_at TEXT,
                 state TEXT NOT NULL DEFAULT 'pending'
                     CHECK (state IN ('pending','confirmed','rolled-back','cancelled')),
                 channel TEXT NOT NULL DEFAULT ''
             );",
        )
        .unwrap();

        // Seed: 2 hosts × 3 dispatches. Per host, the most-recent
        // (offset = -100) is the operational survivor.
        let seed: Vec<(&str, &str, i64, &str)> = vec![
            // (hostname, rollout_id, dispatched_offset_seconds, state)
            ("ohm", "stable@r1", -300, "rolled-back"),
            ("ohm", "stable@r2", -200, "rolled-back"),
            ("ohm", "stable@r3", -100, "confirmed"),
            ("krach", "stable@r1", -300, "rolled-back"),
            ("krach", "stable@r2", -200, "rolled-back"),
            ("krach", "stable@r3", -100, "pending"),
        ];
        for (host, rollout, offset, state) in &seed {
            let dispatched_modifier = format!("{offset} seconds");
            conn.execute(
                "INSERT INTO pending_confirms(
                     hostname, rollout_id, channel, wave,
                     target_closure_hash, target_channel_ref,
                     dispatched_at, confirm_deadline, state
                 )
                 VALUES (?1, ?2, 'stable', 0, 'sys', ?2,
                         datetime('now', ?3),
                         datetime('now', '+120 seconds'),
                         ?4)",
                params![host, rollout, dispatched_modifier, state],
            )
            .unwrap();
        }

        // Apply V006 verbatim.
        let v006 =
            include_str!("../../migrations/V006__split_pending_confirms.sql");
        conn.execute_batch(v006).unwrap();

        // dispatch_history has every legacy row.
        let history_n: i64 = conn
            .query_row("SELECT COUNT(*) FROM dispatch_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(history_n, 6, "every legacy row lands in history");

        // host_dispatch_state has one row per hostname.
        let op_n: i64 = conn
            .query_row("SELECT COUNT(*) FROM host_dispatch_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(op_n, 2);

        // ohm's most recent dispatch is r3 (confirmed).
        let (ohm_rollout, ohm_state): (String, String) = conn
            .query_row(
                "SELECT rollout_id, state FROM host_dispatch_state
                 WHERE hostname = 'ohm'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ohm_rollout, "stable@r3");
        assert_eq!(ohm_state, "confirmed");

        // krach's most recent dispatch is r3 (pending).
        let (krach_rollout, krach_state): (String, String) = conn
            .query_row(
                "SELECT rollout_id, state FROM host_dispatch_state
                 WHERE hostname = 'krach'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(krach_rollout, "stable@r3");
        assert_eq!(krach_state, "pending");

        // Terminal stamps: 4 rolled-back legacy rows → 4 history
        // rows with terminal_state set; the other 2 ('confirmed'
        // and 'pending') stay open.
        let terminal_n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_history
                 WHERE terminal_state IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(terminal_n, 4);
        let rolled_back_n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_history
                 WHERE terminal_state = 'rolled-back'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rolled_back_n, 4);

        // pending_confirms is dropped.
        let pc_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'pending_confirms'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pc_exists, 0, "V006 must drop pending_confirms");
    }
}
