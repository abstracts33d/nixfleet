//! Every 30s, scan `pending_confirms` for past-deadline rows still
//! `'pending'` and transition them to `'rolled-back'`. CP + agent
//! halves work independently — CP marks state regardless of whether
//! the agent's local rollback succeeded.

use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

pub const ROLLBACK_TIMER_INTERVAL: Duration = Duration::from_secs(30);

pub fn spawn(db: Arc<Db>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROLLBACK_TIMER_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match db.confirms().pending_confirms_expired() {
                Ok(expired) if !expired.is_empty() => {
                    let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
                    for (id, hostname, rollout_id, wave, target_closure) in &expired {
                        tracing::info!(
                            target: "rollback",
                            id,
                            hostname = %hostname,
                            rollout = %rollout_id,
                            wave,
                            target_closure = %target_closure,
                            "rolling back: confirm window expired"
                        );
                    }
                    match db.confirms().mark_rolled_back(&ids) {
                        Ok(n) => tracing::debug!(rolled_back = n, "rollback timer: marked"),
                        Err(err) => tracing::warn!(error = %err, "rollback timer: mark failed"),
                    }
                }
                Ok(_) => {
                    tracing::trace!("rollback timer: nothing expired");
                }
                Err(err) => {
                    tracing::warn!(error = %err, "rollback timer: query failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_expired_when_table_empty() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let expired = db.confirms().pending_confirms_expired().unwrap();
        assert!(expired.is_empty());
    }

    // Round-trip integration test lives in db.rs::tests.
}
