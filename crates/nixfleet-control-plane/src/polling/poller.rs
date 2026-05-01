//! Shared spawn/tick/log scaffolding for signed-artifact poll tasks.
//!
//! Both `channel_refs_poll` and `revocations_poll` repeat the same
//! tokio-spawn shape: build one `reqwest::Client`, set up an
//! interval ticker with `MissedTickBehavior::Skip`, loop forever,
//! warn on per-tick error, retain previous artifact state. This
//! module hoists that boilerplate.
//!
//! What stays per-artifact: verify, schema-gate, on-success replay
//! into DB / cache, and the INFO heartbeat that names the
//! artifact-specific fields (entries count, signed_at, ci_commit,
//! changed flag, …). Those are intentionally *not* parameterised
//! through a trait — see signed-sidecar-pattern.md.

use std::time::Duration;

use anyhow::Result;

use super::signed_fetch;

/// Drives one signed-artifact poll task.
///
/// `interval` is the cadence; `label` is the static string that
/// distinguishes this artifact in the per-tick warn log
/// (`channel-refs`, `revocations`, …).
pub struct SignedArtifactPoller {
    pub interval: Duration,
    pub label: &'static str,
}

impl SignedArtifactPoller {
    /// Spawn the poll task. The supplied `tick` closure is invoked
    /// once per `interval`, with a clone of the shared `reqwest::Client`
    /// built when the task starts (the Client is internally Arc'd, so
    /// clone is cheap). The closure returns `Result<()>`:
    ///
    /// - `Ok(())`: closure has already done its work — verify, replay
    ///   into the appropriate DB / cache target, INFO heartbeat. The
    ///   poller does no further logging on success.
    /// - `Err(_)`: the poller logs a warn keyed on `label` and moves
    ///   to the next tick. The closure is responsible for not
    ///   mutating any shared state on the error path — that's what
    ///   "retain previous state" means in practice.
    pub fn spawn<F, Fut>(self, mut tick: F) -> tokio::task::JoinHandle<()>
    where
        F: FnMut(reqwest::Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        tokio::spawn(async move {
            let client = signed_fetch::build_client();

            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;
                if let Err(err) = tick(client.clone()).await {
                    tracing::warn!(
                        target: "polling",
                        label = self.label,
                        error = %err,
                        "poll failed; retaining previous state",
                    );
                }
            }
        })
    }
}
