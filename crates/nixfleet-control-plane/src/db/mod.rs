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

    /// V001 baseline: applying the consolidated schema produces every
    /// expected table and none of the legacy ones the post-#81 cycle
    /// retired (pending_confirms, schema_placeholder). Future schema
    /// changes (V002 onward) add their own per-migration test next to
    /// this one — the migration-equivalence tier from Audit B #14.
    #[test]
    fn v001_produces_consolidated_schema() {
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
        for expected in &[
            "token_replay",
            "cert_revocations",
            "host_dispatch_state",
            "dispatch_history",
            "host_rollout_state",
            "host_reports",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "V001 must create {expected}; got {names:?}",
            );
        }
        for legacy in &["pending_confirms", "schema_placeholder"] {
            assert!(
                !names.contains(&legacy.to_string()),
                "V001 must not carry legacy table {legacy}",
            );
        }
    }


    /// Helper: query the column names of `table` in declaration order.
    /// Kept after the V006-V007 collapse for the next per-migration
    /// test (Audit B #14 migration-equivalence tier) — first new
    /// migration past V001 should add a test here that uses both
    /// helpers.
    #[allow(dead_code)]
    fn columns_of(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// Helper: assert that a table exists in the connected schema.
    /// See `columns_of` for kept-for-future rationale.
    #[allow(dead_code)]
    fn assert_table_exists(conn: &Connection, table: &str) {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "table {table} must exist after migration");
    }
}
