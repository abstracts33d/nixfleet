use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Mutex;

/// SQLite-backed persistence for the control plane.
///
/// Stores generation assignments and agent reports as an audit trail.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Open (or create) the SQLite database at the given path.
    pub fn new(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .context("failed to create database directory")?;
            }
        }

        let conn =
            Connection::open(path).context("failed to open SQLite database")?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Initialize database tables.
    pub fn init(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS generations (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                machine_id TEXT    NOT NULL,
                hash       TEXT    NOT NULL,
                set_at     TEXT    NOT NULL DEFAULT (datetime('now')),
                UNIQUE(machine_id)
            );

            CREATE TABLE IF NOT EXISTS reports (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                machine_id    TEXT    NOT NULL,
                generation    TEXT    NOT NULL,
                success       INTEGER NOT NULL,
                message       TEXT,
                received_at   TEXT    NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_reports_machine
                ON reports(machine_id, received_at DESC);

            CREATE TABLE IF NOT EXISTS machines (
                machine_id    TEXT PRIMARY KEY,
                lifecycle     TEXT NOT NULL DEFAULT 'active',
                registered_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .context("failed to initialize database tables")?;

        Ok(())
    }

    /// Set (upsert) the desired generation for a machine.
    pub fn set_desired_generation(
        &self,
        machine_id: &str,
        hash: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO generations (machine_id, hash)
             VALUES (?1, ?2)
             ON CONFLICT(machine_id) DO UPDATE SET hash = ?2, set_at = datetime('now')",
            rusqlite::params![machine_id, hash],
        )
        .context("failed to set desired generation")?;
        Ok(())
    }

    /// Get the desired generation for a machine, if set.
    pub fn get_desired_generation(
        &self,
        machine_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT hash FROM generations WHERE machine_id = ?1",
            rusqlite::params![machine_id],
            |row| row.get(0),
        );
        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all desired generations (machine_id, hash).
    pub fn list_desired_generations(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT machine_id, hash FROM generations")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Store an agent report.
    pub fn insert_report(
        &self,
        machine_id: &str,
        generation: &str,
        success: bool,
        message: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO reports (machine_id, generation, success, message)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![machine_id, generation, success as i32, message],
        )
        .context("failed to insert report")?;
        Ok(())
    }

    /// Register a machine (upsert) with a given lifecycle state.
    pub fn register_machine(
        &self,
        machine_id: &str,
        lifecycle: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO machines (machine_id, lifecycle)
             VALUES (?1, ?2)
             ON CONFLICT(machine_id) DO UPDATE SET lifecycle = ?2",
            rusqlite::params![machine_id, lifecycle],
        )
        .context("failed to register machine")?;
        Ok(())
    }

    /// Get the lifecycle state for a machine.
    pub fn get_machine_lifecycle(
        &self,
        machine_id: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT lifecycle FROM machines WHERE machine_id = ?1",
            rusqlite::params![machine_id],
            |row| row.get(0),
        );
        match result {
            Ok(lifecycle) => Ok(Some(lifecycle)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update a machine's lifecycle state.
    pub fn set_machine_lifecycle(
        &self,
        machine_id: &str,
        lifecycle: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE machines SET lifecycle = ?2 WHERE machine_id = ?1",
            rusqlite::params![machine_id, lifecycle],
        )
        .context("failed to update machine lifecycle")?;
        Ok(rows > 0)
    }

    /// List all registered machines.
    pub fn list_machines(&self) -> Result<Vec<MachineRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT machine_id, lifecycle, registered_at FROM machines")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MachineRow {
                    machine_id: row.get(0)?,
                    lifecycle: row.get(1)?,
                    registered_at: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get recent reports for a machine (most recent first).
    pub fn get_recent_reports(
        &self,
        machine_id: &str,
        limit: usize,
    ) -> Result<Vec<ReportRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT machine_id, generation, success, message, received_at
             FROM reports
             WHERE machine_id = ?1
             ORDER BY received_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![machine_id, limit], |row| {
                Ok(ReportRow {
                    machine_id: row.get(0)?,
                    generation: row.get(1)?,
                    success: row.get::<_, i32>(2)? != 0,
                    message: row.get(3)?,
                    received_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// A report row as stored in SQLite.
#[derive(Debug, Clone)]
pub struct ReportRow {
    pub machine_id: String,
    pub generation: String,
    pub success: bool,
    pub message: Option<String>,
    pub received_at: String,
}

/// A machine row as stored in SQLite.
#[derive(Debug, Clone)]
pub struct MachineRow {
    pub machine_id: String,
    pub lifecycle: String,
    pub registered_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Db::new(db_path.to_str().unwrap()).unwrap();
        db.init().unwrap();
        (db, dir)
    }

    #[test]
    fn test_init_is_idempotent() {
        let (db, _dir) = make_db();
        db.init().unwrap();
    }

    #[test]
    fn test_set_and_get_desired_generation() {
        let (db, _dir) = make_db();
        db.set_desired_generation("krach", "/nix/store/abc123")
            .unwrap();
        let hash = db.get_desired_generation("krach").unwrap();
        assert_eq!(hash, Some("/nix/store/abc123".to_string()));
    }

    #[test]
    fn test_get_desired_generation_missing() {
        let (db, _dir) = make_db();
        let hash = db.get_desired_generation("nonexistent").unwrap();
        assert!(hash.is_none());
    }

    #[test]
    fn test_set_desired_generation_upsert() {
        let (db, _dir) = make_db();
        db.set_desired_generation("krach", "/nix/store/gen1")
            .unwrap();
        db.set_desired_generation("krach", "/nix/store/gen2")
            .unwrap();
        let hash = db.get_desired_generation("krach").unwrap();
        assert_eq!(hash, Some("/nix/store/gen2".to_string()));
    }

    #[test]
    fn test_list_desired_generations() {
        let (db, _dir) = make_db();
        db.set_desired_generation("krach", "/nix/store/abc")
            .unwrap();
        db.set_desired_generation("ohm", "/nix/store/def").unwrap();
        let gens = db.list_desired_generations().unwrap();
        assert_eq!(gens.len(), 2);
    }

    #[test]
    fn test_insert_and_get_reports() {
        let (db, _dir) = make_db();
        db.insert_report("krach", "/nix/store/abc", true, "deployed")
            .unwrap();
        db.insert_report("krach", "/nix/store/abc", false, "rolled back")
            .unwrap();
        let reports = db.get_recent_reports("krach", 10).unwrap();
        assert_eq!(reports.len(), 2);
        // Both reports present — one success, one failure
        let successes = reports.iter().filter(|r| r.success).count();
        let failures = reports.iter().filter(|r| !r.success).count();
        assert_eq!(successes, 1);
        assert_eq!(failures, 1);
    }

    #[test]
    fn test_reports_limit() {
        let (db, _dir) = make_db();
        for i in 0..5 {
            db.insert_report(
                "krach",
                &format!("/nix/store/gen{i}"),
                true,
                "ok",
            )
            .unwrap();
        }
        let reports = db.get_recent_reports("krach", 2).unwrap();
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn test_reports_isolated_per_machine() {
        let (db, _dir) = make_db();
        db.insert_report("krach", "/nix/store/abc", true, "ok")
            .unwrap();
        db.insert_report("ohm", "/nix/store/def", true, "ok")
            .unwrap();
        let krach_reports = db.get_recent_reports("krach", 10).unwrap();
        let ohm_reports = db.get_recent_reports("ohm", 10).unwrap();
        assert_eq!(krach_reports.len(), 1);
        assert_eq!(ohm_reports.len(), 1);
    }

    #[test]
    fn test_register_machine() {
        let (db, _dir) = make_db();
        db.register_machine("krach", "pending").unwrap();
        let lc = db.get_machine_lifecycle("krach").unwrap();
        assert_eq!(lc, Some("pending".to_string()));
    }

    #[test]
    fn test_register_machine_upsert() {
        let (db, _dir) = make_db();
        db.register_machine("krach", "pending").unwrap();
        db.register_machine("krach", "active").unwrap();
        let lc = db.get_machine_lifecycle("krach").unwrap();
        assert_eq!(lc, Some("active".to_string()));
    }

    #[test]
    fn test_get_machine_lifecycle_missing() {
        let (db, _dir) = make_db();
        let lc = db.get_machine_lifecycle("nonexistent").unwrap();
        assert!(lc.is_none());
    }

    #[test]
    fn test_set_machine_lifecycle() {
        let (db, _dir) = make_db();
        db.register_machine("krach", "active").unwrap();
        let updated = db.set_machine_lifecycle("krach", "maintenance").unwrap();
        assert!(updated);
        let lc = db.get_machine_lifecycle("krach").unwrap();
        assert_eq!(lc, Some("maintenance".to_string()));
    }

    #[test]
    fn test_set_machine_lifecycle_missing() {
        let (db, _dir) = make_db();
        let updated = db.set_machine_lifecycle("nonexistent", "active").unwrap();
        assert!(!updated);
    }

    #[test]
    fn test_list_machines() {
        let (db, _dir) = make_db();
        db.register_machine("krach", "active").unwrap();
        db.register_machine("ohm", "pending").unwrap();
        let machines = db.list_machines().unwrap();
        assert_eq!(machines.len(), 2);
    }
}
