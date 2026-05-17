//! SQLite persistence: rusqlite + refinery, WAL + FK, single `Mutex<Connection>`.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub mod allowed_nonces;
pub mod dispatch_queue;
pub mod event_log;
pub mod host_rollout_records;
pub mod probe_failures;
pub mod quarantined_closures;
pub mod revocations;
pub mod rollouts;
pub mod tokens;

pub use tokens::RecordTokenOutcome;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("conn", &"<sqlite>").finish()
    }
}

impl Db {
    /// Creates parent dirs; enables WAL + FK before migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("set sqlite pragmas")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory SQLite for integration tests; trivial wrapper around
    /// `Connection::open_in_memory()` exposed as a public API so
    /// integration tests under `tests/` can construct an isolated `Db`.
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

    /// Idempotent.
    pub fn migrate(&self) -> Result<()> {
        let mut guard = self.conn()?;
        embedded::migrations::runner()
            .run(&mut *guard)
            .context("run sqlite migrations")?;
        Ok(())
    }

    pub fn tokens(&self) -> tokens::Tokens<'_> {
        tokens::Tokens { conn: &self.conn }
    }

    pub fn probe_failures(&self) -> probe_failures::ProbeFailures<'_> {
        probe_failures::ProbeFailures { conn: &self.conn }
    }

    /// Hard state.
    pub fn revocations(&self) -> revocations::Revocations<'_> {
        revocations::Revocations { conn: &self.conn }
    }

    pub fn quarantined_closures(&self) -> quarantined_closures::QuarantinedClosures<'_> {
        quarantined_closures::QuarantinedClosures { conn: &self.conn }
    }

    pub fn rollouts(&self) -> rollouts::Rollouts<'_> {
        rollouts::Rollouts { conn: &self.conn }
    }

    pub fn dispatch_queue(&self) -> dispatch_queue::DispatchQueue<'_> {
        dispatch_queue::DispatchQueue { conn: &self.conn }
    }

    pub fn event_log(&self) -> event_log::EventLog<'_> {
        event_log::EventLog { conn: &self.conn }
    }

    pub fn host_rollout_records(&self) -> host_rollout_records::HostRolloutRecords<'_> {
        host_rollout_records::HostRolloutRecords { conn: &self.conn }
    }
}

/// Surfaces mutex poisoning as anyhow rather than panic.
pub(crate) fn lock_conn(mu: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>> {
    mu.lock()
        .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
}

/// Lock + read. Closure receives a borrowed `Connection`; lock is held
/// for the closure's duration.
pub(crate) fn read<F, T>(mu: &Mutex<Connection>, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T>,
{
    let guard = lock_conn(mu)?;
    f(&guard)
}

/// Lock + open txn + run closure + commit. `label` shapes the begin/commit
/// error context. Closure errors abort the txn.
pub(crate) fn txn<F, T>(mu: &Mutex<Connection>, label: &'static str, f: F) -> Result<T>
where
    F: FnOnce(&rusqlite::Transaction) -> Result<T>,
{
    let mut guard = lock_conn(mu)?;
    let tx = guard
        .transaction()
        .with_context(|| format!("begin {label} txn"))?;
    let v = f(&tx)?;
    tx.commit().with_context(|| format!("commit {label} txn"))?;
    Ok(v)
}

#[cfg(test)]
pub(crate) mod test_helpers;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_produce_consolidated_schema() {
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
            "cert_revocations",
            "dispatch_queue",
            "event_log",
            "host_rollout_records",
            "probe_failures",
            "quarantined_closures",
            "rollouts",
            "token_replay",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "v0.2 baseline must create {expected}; got {names:?}",
            );
        }
        // v0.1 / 9a-deleted tables must not resurface.
        for legacy in &[
            "host_dispatch_state",
            "host_rollout_state",
            "host_reports",
            "dispatch_history",
            "pending_confirms",
            "schema_placeholder",
        ] {
            assert!(
                !names.contains(&legacy.to_string()),
                "v0.2 baseline must not carry legacy table {legacy}",
            );
        }
    }

    #[allow(dead_code)]
    fn columns_of(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

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
