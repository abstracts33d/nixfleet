//! Magic rollback deadline tracker (Phase 4 PR-B).
//!
//! Periodic background task: every 30s, scan `pending_confirms` for
//! rows whose `confirm_deadline` has passed but `state` is still
//! `'pending'`. Transition each to `'rolled-back'` and emit a
//! journal line. The agent learns the rollout was rolled back via
//! its next `/v1/agent/checkin` (the CP would normally include
//! `target = null` and a separate signal — Phase 4 dispatch loop
//! adds that signal).
//!
//! This task is the CP-side half of magic rollback (issue #2). The
//! agent-side half is in `nixfleet-agent`'s activation loop (parallel
//! PR): on a missed confirm window, the agent locally runs
//! `nixos-rebuild --rollback` to revert to the previous boot
//! generation. Both halves work independently — the CP marks state
//! regardless of whether the agent successfully rolled back, so the
//! operator's view via the CP's audit trail is always correct.

use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

/// How often the timer wakes up. 30s matches the reconcile-loop
/// cadence (D2). Faster means quicker detection of missed confirms;
/// slower reduces journal noise. 30s is a fine default for the
/// homelab fleet.
pub const ROLLBACK_TIMER_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn the periodic rollback-timer task. Runs forever; logs at
/// info on each rollback transition, debug otherwise.
pub fn spawn(db: Arc<Db>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(ROLLBACK_TIMER_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            match db.pending_confirms_expired() {
                Ok(expired) if !expired.is_empty() => {
                    let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
                    for (id, hostname, rollout_id, wave, target_closure) in &expired {
                        // One journal line per transition so an
                        // operator can grep `journalctl -u
                        // nixfleet-control-plane | grep rolled-back`
                        // and see exactly which (host, rollout, wave,
                        // closure) failed.
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
                    match db.mark_rolled_back(&ids) {
                        Ok(n) => tracing::debug!(rolled_back = n, "rollback timer: marked"),
                        Err(err) => tracing::warn!(error = %err, "rollback timer: mark failed"),
                    }
                }
                Ok(_) => {
                    // No expired pending confirms — quiet path.
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
        let expired = db.pending_confirms_expired().unwrap();
        assert!(expired.is_empty());
    }

    // A round-trip test (insert past-deadline row → expired returns
    // it → mark_rolled_back transitions it) lives in db.rs's tests
    // module, where it can access the private `conn` helper for
    // synthetic inserts. PR-A's `record_pending_confirm` accessor
    // gives a public path; once both PRs land, the integration test
    // can move here using that.
}
