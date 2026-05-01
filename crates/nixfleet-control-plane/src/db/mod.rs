//! SQLite persistence (rusqlite + refinery, WAL + FK).
//!
//! A single `Mutex<Connection>` is sufficient for fleet sizes ≤ 150
//! hosts (ADR-012); the migration trigger and pool target are
//! documented there. Schema lives under `migrations/`; `migrate()` is
//! idempotent + version-tracked. Mutex poisoning surfaces as anyhow
//! errors.
//!
//! Per-table operations live in submodules and are reached via
//! accessors on [`Db`]: `tokens()`, `confirms()`, `rollout_state()`,
//! `reports()`, `revocations()`. Each submodule's file header names
//! the recovery class (soft vs hard) per ARCHITECTURE.md §6 Phase 10.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub mod confirms;
pub mod reports;
pub mod revocations;
pub mod rollout_state;
pub mod tokens;

pub use confirms::{ExpiredPendingConfirm, PendingConfirmInsert};
pub use reports::{HostReportInsert, HostReportRow};
pub use rollout_state::RolloutDbSnapshot;

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

    /// `pending_confirms` accessor (soft state).
    pub fn confirms(&self) -> confirms::Confirms<'_> {
        confirms::Confirms { conn: &self.conn }
    }

    /// `host_rollout_state` accessor (soft state); also owns the
    /// joined `active_rollouts_snapshot` projection.
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
        assert!(names.contains(&"pending_confirms".to_string()));
        assert!(names.contains(&"host_rollout_state".to_string()));
    }
}
