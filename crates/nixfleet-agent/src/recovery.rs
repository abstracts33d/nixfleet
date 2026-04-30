//! Boot-time recovery for fire-and-forget activation.
//!
//! The fire-and-forget pattern (see `crates/nixfleet-agent/src/activation.rs`)
//! commonly lets the agent be SIGTERMed mid-poll: the new closure restarts
//! `nixfleet-agent.service`, the agent dies, but `nixfleet-switch.service`
//! continues independently and lands the activation. The post-self-switch
//! agent boots into the new closure and needs to know "what was I dispatching"
//! to acknowledge the activation to the CP — otherwise the CP runs out the
//! confirm deadline and rolls a successful host back.
//!
//! `run_boot_recovery` is the agent-startup hook that closes that gap. It
//! reads `<state-dir>/last_dispatched` (written by main before firing) and
//! compares to the current `/run/current-system` basename. Match → post the
//! retroactive `/v1/agent/confirm`. Mismatch → clear the stale record and
//! let the regular checkin loop re-decide.
//!
//! Extracted from `main.rs` so the decision logic is unit-testable: the
//! `current_closure` is passed in rather than read from the live filesystem,
//! so tests inject synthetic values.

use std::path::Path;

use nixfleet_proto::agent_wire::EvaluatedTarget;

use crate::{activation, checkin_state, comms};

/// Outcome of the recovery decision. Exposed so tests can assert on
/// the branch taken without observing side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// No `last_dispatched` record exists — first boot or no
    /// in-flight dispatch from a prior agent run.
    NoRecord,
    /// `current_closure` couldn't be read (e.g. test harness with
    /// no `/run/current-system`). Recovery was skipped.
    NoCurrent,
    /// `current_closure != last_dispatched.closure_hash` — the
    /// dispatch never landed (or rolled back). Stale record cleared,
    /// next regular checkin will re-decide.
    StaleClearedMismatch,
    /// `current == last_dispatched.closure_hash` — the self-switch
    /// landed. Posted the retroactive `/v1/agent/confirm`. The
    /// `confirm_outcome` distinguishes Acknowledged / Cancelled / etc.
    PostedConfirm {
        confirm_outcome: comms::ConfirmOutcome,
    },
    /// Posted the retroactive confirm but the POST itself errored.
    /// Record left in place; next cycle retries.
    PostedConfirmFailed { error: String },
}

/// Run the boot-recovery decision flow. Returns the action taken so
/// callers (tests, future operator-status surfaces) can introspect.
///
/// Best-effort: failures inside the function are logged, never
/// propagated. The main loop's regular checkin cadence is the safety
/// net — total recovery failure means "agent eventually re-converges
/// via dispatch".
pub async fn run_boot_recovery(
    client: &reqwest::Client,
    state_dir: &Path,
    cp_url: &str,
    hostname: &str,
    current_closure: Option<String>,
) -> anyhow::Result<()> {
    let action = decide_and_run(client, state_dir, cp_url, hostname, current_closure).await;
    match &action {
        RecoveryAction::NoRecord => {
            tracing::debug!("boot-recovery: no last_dispatched record (steady-state)");
        }
        RecoveryAction::NoCurrent => {
            tracing::warn!("boot-recovery: skipped — could not read current closure");
        }
        RecoveryAction::StaleClearedMismatch => {
            tracing::info!("boot-recovery: cleared stale dispatch record (current/dispatched mismatch)");
        }
        RecoveryAction::PostedConfirm { confirm_outcome } => {
            tracing::info!(
                outcome = ?confirm_outcome,
                "boot-recovery: retroactive confirm posted",
            );
        }
        RecoveryAction::PostedConfirmFailed { error } => {
            tracing::warn!(
                error = %error,
                "boot-recovery: retroactive confirm POST failed; record retained",
            );
        }
    }
    Ok(())
}

async fn decide_and_run(
    client: &reqwest::Client,
    state_dir: &Path,
    cp_url: &str,
    hostname: &str,
    current_closure: Option<String>,
) -> RecoveryAction {
    let dispatched = match checkin_state::read_last_dispatched(state_dir) {
        Ok(Some(rec)) => rec,
        Ok(None) => return RecoveryAction::NoRecord,
        Err(err) => {
            tracing::warn!(
                error = %err,
                state_dir = %state_dir.display(),
                "boot-recovery: read_last_dispatched failed; treating as absent",
            );
            return RecoveryAction::NoRecord;
        }
    };

    let current = match current_closure {
        Some(c) => c,
        None => return RecoveryAction::NoCurrent,
    };

    if current != dispatched.closure_hash {
        let _ = checkin_state::clear_last_dispatched(state_dir);
        return RecoveryAction::StaleClearedMismatch;
    }

    // Match — post retroactive confirm.
    let boot_id = crate::host_facts::boot_id().unwrap_or_else(|_| "unknown".to_string());
    // Synthesize an EvaluatedTarget shape from the persisted record.
    // signed_at + freshness_window_secs deliberately None: the
    // freshness gate already passed when we first dispatched, and
    // the agent's confirm path doesn't re-check those fields.
    let synthetic_target = EvaluatedTarget {
        closure_hash: dispatched.closure_hash.clone(),
        channel_ref: dispatched.channel_ref.clone(),
        evaluated_at: dispatched.dispatched_at,
        rollout_id: dispatched.rollout_id.clone(),
        wave_index: None,
        activate: None,
        signed_at: None,
        freshness_window_secs: None,
        compliance_mode: None,
    };

    match activation::confirm_target(
        client,
        cp_url,
        hostname,
        &synthetic_target,
        &dispatched.channel_ref,
        /* wave */ 0,
        &boot_id,
    )
    .await
    {
        Ok(outcome) => {
            // On Acknowledged: write last_confirmed + clear dispatch.
            // On Cancelled: CP already rolled this back via deadline;
            // the system is on the new closure but CP says not.
            // Run rollback to converge.
            // On Other: leave record in place; next cycle retries.
            match outcome {
                comms::ConfirmOutcome::Acknowledged => {
                    if let Err(err) = checkin_state::write_last_confirmed(
                        state_dir,
                        &dispatched.closure_hash,
                        chrono::Utc::now(),
                    ) {
                        tracing::warn!(
                            error = %err,
                            "boot-recovery: write_last_confirmed failed (non-fatal)",
                        );
                    }
                    let _ = checkin_state::clear_last_dispatched(state_dir);
                }
                comms::ConfirmOutcome::Cancelled => {
                    let _ = activation::rollback().await;
                    let _ = checkin_state::clear_last_dispatched(state_dir);
                }
                comms::ConfirmOutcome::Other => {
                    // Leave record in place.
                }
            }
            RecoveryAction::PostedConfirm {
                confirm_outcome: outcome,
            }
        }
        Err(err) => RecoveryAction::PostedConfirmFailed {
            error: err.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkin_state::LastDispatchRecord;
    use chrono::Utc;
    use tempfile::TempDir;

    fn dummy_client() -> reqwest::Client {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap()
    }

    fn sample_record(closure: &str) -> LastDispatchRecord {
        LastDispatchRecord {
            closure_hash: closure.to_string(),
            channel_ref: "stable@deadbeef".to_string(),
            rollout_id: Some("stable@deadbeef".to_string()),
            dispatched_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn no_record_when_state_dir_empty() {
        let dir = TempDir::new().unwrap();
        let action = decide_and_run(
            &dummy_client(),
            dir.path(),
            "https://cp:0",
            "test-host",
            Some("any-closure".to_string()),
        )
        .await;
        assert_eq!(action, RecoveryAction::NoRecord);
    }

    #[tokio::test]
    async fn no_current_when_current_closure_missing() {
        let dir = TempDir::new().unwrap();
        checkin_state::write_last_dispatched(dir.path(), &sample_record("some-closure")).unwrap();
        let action = decide_and_run(
            &dummy_client(),
            dir.path(),
            "https://cp:0",
            "test-host",
            None,
        )
        .await;
        assert_eq!(action, RecoveryAction::NoCurrent);
        // Record should still be there for the next attempt.
        assert!(checkin_state::read_last_dispatched(dir.path())
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn mismatch_clears_stale_record() {
        let dir = TempDir::new().unwrap();
        checkin_state::write_last_dispatched(dir.path(), &sample_record("dispatched-closure"))
            .unwrap();
        let action = decide_and_run(
            &dummy_client(),
            dir.path(),
            "https://cp:0",
            "test-host",
            Some("different-closure".to_string()),
        )
        .await;
        assert_eq!(action, RecoveryAction::StaleClearedMismatch);
        assert!(checkin_state::read_last_dispatched(dir.path())
            .unwrap()
            .is_none(),
            "stale record must be cleared on mismatch",
        );
    }

    #[tokio::test]
    async fn match_attempts_post_and_records_failure_on_unreachable_cp() {
        let dir = TempDir::new().unwrap();
        checkin_state::write_last_dispatched(dir.path(), &sample_record("matching-closure"))
            .unwrap();
        // CP URL points at an unreachable port. Recovery should attempt
        // the POST, hit a transport error, and surface as PostedConfirmFailed.
        let action = decide_and_run(
            &dummy_client(),
            dir.path(),
            "https://127.0.0.1:1/",
            "test-host",
            Some("matching-closure".to_string()),
        )
        .await;
        match action {
            RecoveryAction::PostedConfirmFailed { error } => {
                assert!(!error.is_empty(), "transport error should carry a message");
            }
            other => panic!("expected PostedConfirmFailed, got {other:?}"),
        }
        // Record should remain so the next cycle can retry.
        assert!(
            checkin_state::read_last_dispatched(dir.path())
                .unwrap()
                .is_some(),
            "unfailed POST must leave the record for the next checkin to retry",
        );
    }
}
