//! Post-switch verify: poll `/run/current-system` until it resolves
//! to the expected basename, or one of the terminal `PollOutcome`
//! conditions fires.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use super::outcome::{POLL_BUDGET, POLL_INTERVAL};

pub(super) async fn read_current_system_basename() -> Result<String> {
    let target = tokio::fs::read_link("/run/current-system")
        .await
        .with_context(|| "readlink /run/current-system")?;
    let basename = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!(
                "/run/current-system target has no utf-8 basename: {}",
                target.display()
            )
        })?
        .to_string();
    Ok(basename)
}

/// Result of polling `/run/current-system` for the expected basename.
#[derive(Debug, Clone)]
pub enum PollOutcome {
    /// Symlink resolved to the expected basename within the budget.
    Settled,
    /// Budget elapsed without ever observing the expected basename.
    /// `last_observed` distinguishes "switch is still running, just
    /// slow" from "switch died and the symlink is unchanged".
    Timeout { last_observed: String },
    /// Observed `/run/current-system` resolving to a basename that is
    /// neither the expected new closure nor the pre-switch basename.
    /// Indicates an activation script (or concurrent process) pointed
    /// the symlink somewhere we never asked for. Caller must roll back.
    /// Only produced when the caller set `previous_basename = Some(_)`.
    FlippedToUnexpected { observed: String },
}

/// Configuration + execution surface for polling
/// `/run/current-system` until it resolves to the expected closure
/// basename, or one of the terminal `PollOutcome` conditions fires.
///
/// When `previous_basename` is `Some(p)`, observing a basename that
/// is neither `expected_basename` nor `p` is treated as a hard
/// mismatch and returned as `PollOutcome::FlippedToUnexpected`
/// immediately — the system cannot legitimately be at any third
/// basename mid-switch. Leave it as `None` for the rollback path,
/// where a stable pre-state reference isn't meaningful and any
/// non-match collapses into the timeout branch.
///
/// Read errors during polling are non-fatal: the symlink may be
/// briefly absent during activation. The timer keeps running.
pub struct VerifyPoll<'a> {
    pub expected_basename: &'a str,
    pub previous_basename: Option<&'a str>,
    pub interval: Duration,
    pub budget: Duration,
}

impl<'a> VerifyPoll<'a> {
    /// Defaults: `POLL_BUDGET` / `POLL_INTERVAL`, no `previous_basename`.
    pub fn new(expected_basename: &'a str) -> Self {
        Self {
            expected_basename,
            previous_basename: None,
            interval: POLL_INTERVAL,
            budget: POLL_BUDGET,
        }
    }

    /// Enable flip-to-unexpected detection by pinning the pre-switch
    /// basename. Builder-style so call sites stay one expression.
    pub fn with_previous(mut self, previous: &'a str) -> Self {
        self.previous_basename = Some(previous);
        self
    }

    /// Poll until the symlink resolves to `expected_basename` or the
    /// budget elapses. Pure — no logging, deterministic timing — so
    /// it's straightforward to test.
    pub async fn until_settled(&self) -> PollOutcome {
        let deadline = tokio::time::Instant::now() + self.budget;
        // Initial None is dead in every iteration of the loop body
        // (Ok/Err branches both assign before the deadline check), but
        // it's the natural type for "no read has completed yet" and
        // we keep the unwrap_or_else fallback for the budget=0 edge.
        #[allow(unused_assignments)]
        let mut last_observed: Option<String> = None;

        loop {
            match read_current_system_basename().await {
                Ok(basename) => {
                    if basename == self.expected_basename {
                        return PollOutcome::Settled;
                    }
                    if let Some(prev) = self.previous_basename {
                        if basename != prev {
                            return PollOutcome::FlippedToUnexpected {
                                observed: basename,
                            };
                        }
                    }
                    last_observed = Some(basename);
                }
                Err(err) => {
                    // Symlink missing or unreadable. Capture the error
                    // message so the timeout diagnostic surfaces what
                    // happened, but keep polling.
                    last_observed = Some(format!("<read-error: {err}>"));
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return PollOutcome::Timeout {
                    last_observed: last_observed
                        .unwrap_or_else(|| String::from("<no-reads-completed>")),
                };
            }
            tokio::time::sleep(self.interval).await;
        }
    }
}
