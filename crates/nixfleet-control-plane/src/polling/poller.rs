//! Shared spawn/tick/log scaffolding for signed-artifact poll tasks.

use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use super::signed_fetch;

pub struct SignedArtifactPoller {
    pub interval: Duration,
    pub label: &'static str,
}

impl SignedArtifactPoller {
    /// Closure must not mutate shared state on its error path; poller logs a warn and retries.
    pub fn spawn<F, Fut>(self, cancel: CancellationToken, mut tick: F) -> tokio::task::JoinHandle<()>
    where
        F: FnMut(reqwest::Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send,
    {
        tokio::spawn(async move {
            let client = signed_fetch::build_client();

            let mut ticker = tokio::time::interval(self.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!(
                            target: "shutdown",
                            label = self.label,
                            "poll task shut down",
                        );
                        return;
                    }
                    _ = ticker.tick() => {
                        if let Err(err) = tick(client.clone()).await {
                            tracing::warn!(
                                target: "polling",
                                label = self.label,
                                error = %err,
                                "poll failed; retaining previous state",
                            );
                        }
                    }
                }
            }
        })
    }
}
