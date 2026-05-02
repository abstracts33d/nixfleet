//! Main activate pipeline: realise → set-profile → fire (backend) →
//! poll → self-correct. Single attempt per call; retry comes from
//! the agent's main poll loop.

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

use super::backend::ActivationBackend;
use super::outcome::ActivationOutcome;
use super::profile::self_correct_profile;
use super::realise::{realise, RealiseError};
use super::verify_poll::{read_current_system_basename, PollOutcome, VerifyPoll};

/// Generic-over-backend form. Production calls `activate(target)`
/// (the façade in `mod.rs`); tests inject a fake to assert
/// per-platform behaviour without shelling out.
pub async fn activate_with<B: ActivationBackend>(
    backend: &B,
    target: &EvaluatedTarget,
) -> Result<ActivationOutcome> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target",
    );

    // Step 0: bow out if another switch-to-configuration is in
    // flight (operator manual run, sibling Ansible play, etc.) —
    // racing on the same lock produces interleaved logs + spurious
    // SwitchFailed timeouts even when the other switch succeeds.
    if backend.is_switch_in_progress().await {
        tracing::info!(
            target_closure = %target.closure_hash,
            "agent: skipping activation — another switch-to-configuration is in flight",
        );
        return Ok(ActivationOutcome::RealiseFailed {
            reason: "switch-to-configuration lock held by another process; will retry on next tick".to_string(),
        });
    }

    // Step 1: realise. Forces fetch + sig verify explicitly; we assert
    // the realised path matches the requested one to catch symlink /
    // substitution-redirect surprises.
    let store_path = format!("/nix/store/{}", target.closure_hash);
    let realised = match realise(&store_path).await {
        Ok(p) => p,
        Err(RealiseError::SignatureMismatch { stderr_tail }) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                stderr_tail = %stderr_tail,
                "agent: closure signature mismatch — refused by nix substituter trust",
            );
            return Ok(ActivationOutcome::SignatureMismatch {
                closure_hash: target.closure_hash.clone(),
                stderr_tail,
            });
        }
        Err(RealiseError::Other(err)) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: realisation failed; not switching",
            );
            return Ok(ActivationOutcome::RealiseFailed {
                reason: err.to_string(),
            });
        }
    };

    if realised != store_path {
        tracing::error!(
            target_closure = %target.closure_hash,
            requested = %store_path,
            realised = %realised,
            "agent: nix-store --realise returned an unexpected path; not switching",
        );
        return Ok(ActivationOutcome::RealiseFailed {
            reason: format!(
                "realised path {realised} does not match requested {store_path}",
            ),
        });
    }

    // Step 2: set the system profile FIRST so the bootloader follows
    // the new closure even if the switch process dies mid-run. The
    // activation script also sets the profile, but doing it here
    // closes the crash window between fire and script-profile-bump.
    let set_status = Command::new("nix-env")
        .arg("--profile")
        .arg("/nix/var/nix/profiles/system")
        .arg("--set")
        .arg(&store_path)
        .status()
        .await
        .with_context(|| "spawn nix-env --set")?;

    if !set_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?set_status.code(),
            "agent: nix-env --set failed; not running switch-to-configuration",
        );
        return Ok(ActivationOutcome::SwitchFailed {
            phase: "nix-env-set".to_string(),
            exit_code: set_status.code(),
        });
    }

    // Capture pre-switch basename so the post-fire poll can
    // distinguish "still pre-flip, switch is slow" (basename ==
    // previous) from "flipped to a third path we never asked for"
    // (basename ∉ {expected, previous}). Read failure here aborts
    // before we fire — without a baseline we can't validate the
    // post-state.
    let previous_basename = match read_current_system_basename().await {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: cannot read /run/current-system pre-switch; aborting activation",
            );
            return Ok(ActivationOutcome::RealiseFailed {
                reason: format!("pre-switch /run/current-system read failed: {err}"),
            });
        }
    };

    // Step 3: fire (backend-dispatched fire-and-forget). See
    // `LinuxBackend::fire_switch` / `DarwinBackend::fire_switch`
    // for the per-platform detail.
    if let Some(outcome) = backend.fire_switch(target, &store_path).await? {
        return Ok(outcome);
    }

    // Step 4: poll. If the agent gets killed mid-poll (new closure
    // stops nixfleet-agent.service), `nixfleet-switch.service`
    // continues independently and the post-switch agent's
    // boot-recovery path posts the retroactive confirm.
    let expected = &target.closure_hash;
    match VerifyPoll::new(expected)
        .with_previous(&previous_basename)
        .until_settled()
        .await
    {
        PollOutcome::Settled => {
            // Step 5: profile self-correction. Defends against the
            // activation script (or a concurrent nix-env) re-pointing
            // `/nix/var/nix/profiles/system` after our Step 2 set —
            // current-system would match but the bootloader pointer
            // would be off, surprising us on next reboot.
            if let Err(err) = self_correct_profile(&store_path).await {
                tracing::warn!(
                    error = %err,
                    "agent: profile self-correction failed (non-fatal); current-system OK so activation continues",
                );
            }
            tracing::info!(
                target_closure = %expected,
                "agent: activation fire-and-forget complete (poll observed expected closure)",
            );
            Ok(ActivationOutcome::FiredAndPolled)
        }
        PollOutcome::Timeout { last_observed } => {
            // Best-effort triage: unit may still be running (large
            // download); ExecMainStatus inconclusive in that case.
            let exit_code = backend.read_unit_exit_code("nixfleet-switch.service").await;
            tracing::error!(
                target_closure = %expected,
                last_observed = %last_observed,
                exit_code = ?exit_code,
                "agent: switch poll timed out — declaring SwitchFailed",
            );
            Ok(ActivationOutcome::SwitchFailed {
                phase: "switch-poll-timeout".to_string(),
                exit_code,
            })
        }
        PollOutcome::FlippedToUnexpected { observed } => {
            tracing::error!(
                target_closure = %expected,
                actual = %observed,
                previous = %previous_basename,
                "agent: post-switch verify caught flip to unexpected closure — rolling back",
            );
            Ok(ActivationOutcome::VerifyMismatch {
                expected: expected.clone(),
                actual: observed,
            })
        }
    }
}
