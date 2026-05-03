//! Boot-time recovery: a post-self-switch agent reads `last_dispatched` to
//! retroactively confirm the in-flight target before its deadline expires.

use std::path::Path;

use nixfleet_proto::agent_wire::EvaluatedTarget;

use crate::{activation, checkin_state, comms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    NoRecord,
    NoCurrent,
    StaleClearedMismatch,
    PostedConfirm {
        confirm_outcome: comms::ConfirmOutcome,
    },
    PostedConfirmFailed { error: String },
}

/// Best-effort: failures are logged, never propagated; main poll re-converges.
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

    let boot_id = crate::host_facts::boot_id().unwrap_or_else(|_| "unknown".to_string());
    // LOADBEARING: signed_at/freshness_window_secs None — freshness already passed at dispatch.
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
                    // LOADBEARING: rollback failure must NOT clear last_dispatched (clearing splits brain).
                    // GOTCHA: rollback() returns Ok(Failed) for in-band failure — inspect outcome, not just Result.
                    match activation::rollback().await {
                        Ok(outcome) if outcome.success() => {
                            let _ = checkin_state::clear_last_dispatched(state_dir);
                        }
                        Ok(outcome) => {
                            tracing::error!(
                                phase = ?outcome.phase(),
                                exit_code = ?outcome.exit_code(),
                                "boot-recovery: rollback FAILED — leaving last_dispatched in place for next-boot retry",
                            );
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "boot-recovery: rollback errored — leaving last_dispatched in place for next-boot retry",
                            );
                        }
                    }
                }
                comms::ConfirmOutcome::Other => {}
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
        assert!(
            checkin_state::read_last_dispatched(dir.path())
                .unwrap()
                .is_some(),
            "unfailed POST must leave the record for the next checkin to retry",
        );
    }
}
