//! Event-log writer task: dedicated consumer of [`EventLogEntry`] values
//! the reducer-task applier emits, persisting them to SQLite outside the
//! reducer's critical section.
//!
//! The applier hands entries to this task via [`EventLogTx`]. The reducer
//! never touches `Db::event_log()` directly — that keeps the reducer task
//! free of the SQLite Mutex during high-frequency `RemoteAppendEventLog` /
//! `RecordTransition` effects, and isolates writer hiccups (disk fsync
//! pauses, mutex contention) from the per-host `step()` path.
//!
//! Backpressure: the channel is bounded. When full, the applier's
//! `send().await` waits — which surfaces the slowdown back into the
//! reducer's input MPSC, preserving the no-fail-open contract for the
//! audit log (RFC-0005 §6: every gate decision and state transition
//! must reach the log; silently dropping is forbidden).

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::ShutdownToken;
use crate::db::Db;
use crate::db::event_log::EventLogEntry;

/// Bounded channel depth for event-log entries between the reducer applier
/// and the writer task.
///
/// Sizing rationale (v0.2 homelab scale):
/// - ~256-host fleet, ~10 events per host during peak rollout activation,
///   ⇒ peak burst ~2 560 entries; sustained rate is much lower.
/// - SQLite + WAL sustained insert rate is ~10 k/s on stock SSDs, so
///   draining 2 560 entries is < 300 ms.
/// - Half of peak burst is comfortable headroom without making the writer
///   queue itself a memory sink during steady state. 1 024 entries × ~256
///   bytes/entry ≈ 256 KiB tail latency budget.
/// - Backpressure surfaces at the reducer (its `send().await` waits),
///   which is the desired behaviour: the audit log must not lose entries.
pub const EVENT_LOG_CHANNEL_CAPACITY: usize = 1024;

pub type EventLogTx = mpsc::Sender<EventLogEntry>;
pub type EventLogRx = mpsc::Receiver<EventLogEntry>;

/// Spawn the writer task. Returns the `JoinHandle` for the runtime drain.
pub fn spawn(db: Arc<Db>, mut rx: EventLogRx, shutdown: ShutdownToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    drain_pending(&db, &mut rx);
                    tracing::info!(
                        target: "shutdown",
                        task = "cp_event_log_writer",
                        "task shut down (drained pending entries)",
                    );
                    return;
                }
                maybe_entry = rx.recv() => {
                    let Some(entry) = maybe_entry else {
                        // All senders dropped (runtime tearing down).
                        return;
                    };
                    if let Err(err) = db.event_log().append(&entry) {
                        tracing::error!(
                            target: "cp_runtime",
                            kind = ?entry.kind,
                            host_id = ?entry.host_id,
                            rollout_id = ?entry.rollout_id,
                            error = %err,
                            "event_log writer: append failed",
                        );
                        // Continue — losing one row is preferable to
                        // killing the writer and silently dropping every
                        // subsequent entry.
                    }
                }
            }
        }
    })
}

/// Best-effort drain of in-flight entries on shutdown. Skips `send().await`
/// because we're past the cancellation point — no new entries can arrive
/// (every applier holds a `Sender` that's already been dropped or is
/// about to be).
fn drain_pending(db: &Db, rx: &mut EventLogRx) {
    while let Ok(entry) = rx.try_recv() {
        if let Err(err) = db.event_log().append(&entry) {
            tracing::warn!(
                target: "cp_runtime",
                kind = ?entry.kind,
                error = %err,
                "event_log writer drain: append failed",
            );
        }
    }
}
