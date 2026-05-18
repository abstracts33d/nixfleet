//! Periodic timer that emits [`super::super::ReducerInput::AgentAdvanceTick`]
//! at a fixed interval. Mirror of the CP runtime's `PlanTick`.
//!
//! Cadence: 5s, intentionally tighter than CP's 15s `PLAN_TICK_INTERVAL`
//! because sustained-failure detection on the agent is more time-
//! critical than channel-edges checks on the CP. A late tick means
//! Soaking → Failed transitions lag by ≤5s; that's acceptable per
//! RFC-0005 §6's threshold semantics.

use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::{ReducerInput, ShutdownToken};

const TICK_INTERVAL: Duration = Duration::from_secs(5);

pub fn spawn(
    _clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + TICK_INTERVAL, TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_advance_ticker",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(err) = input_tx.send(ReducerInput::AgentAdvanceTick).await {
                        tracing::warn!(
                            target: "agent_advance_ticker",
                            error = %err,
                            "reducer input channel closed",
                        );
                        return;
                    }
                }
            }
        }
    })
}
