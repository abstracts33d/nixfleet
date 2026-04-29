//! Periodic SQLite hygiene sweep.
//!
//! Every hour, walks the soft-state tables that accumulate without
//! their own retention semantics:
//!
//! - `token_replay` — bootstrap nonces past the 24h validity window
//!   (`Db::prune_token_replay`)
//! - `pending_confirms` — terminal rows (`rolled-back` / `cancelled`)
//!   past 7 days (`Db::prune_pending_confirms`, )
//! - `host_reports` — event log past 7 days (`Db::prune_host_reports`,
//!   )
//!
//! All helpers are idempotent — the task can be killed at any tick
//! boundary without losing semantics. Mirrors the rollback-timer
//! shape so operators see `prune` lines in the same JSON-line journal
//! they already follow.

use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

const TICK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const TOKEN_REPLAY_RETENTION_HOURS: i64 = 24;
const PENDING_CONFIRMS_RETENTION_HOURS: i64 = 24 * 7;
const HOST_REPORTS_RETENTION_HOURS: i64 = 24 * 7;

/// Spawn the periodic prune task. Runs forever; one INFO line per
/// tick summarising what was pruned. Failures are non-fatal — the
/// task logs a warn + continues with the next tick.
pub fn spawn(db: Arc<Db>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            let token_pruned = match db.prune_token_replay(TOKEN_REPLAY_RETENTION_HOURS) {
                Ok(n) => n,
                Err(err) => {
                    tracing::warn!(error = %err, "prune timer: token_replay failed");
                    0
                }
            };
            let pending_pruned =
                match db.prune_pending_confirms(PENDING_CONFIRMS_RETENTION_HOURS) {
                    Ok(n) => n,
                    Err(err) => {
                        tracing::warn!(error = %err, "prune timer: pending_confirms failed");
                        0
                    }
                };
            let reports_pruned =
                match db.prune_host_reports(HOST_REPORTS_RETENTION_HOURS) {
                    Ok(n) => n,
                    Err(err) => {
                        tracing::warn!(error = %err, "prune timer: host_reports failed");
                        0
                    }
                };
            tracing::info!(
                target: "prune",
                token_replay = token_pruned,
                pending_confirms = pending_pruned,
                host_reports = reports_pruned,
                "prune timer: hourly sweep complete",
            );
        }
    })
}
