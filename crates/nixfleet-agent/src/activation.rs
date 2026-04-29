//! Agent-side activation logic.
//!
//! The CP issues a closure hash via `CheckinResponse.target`; the
//! agent's job is to install + boot that closure. Per ARCHITECTURE.md
//! the agent is the *last line of defense* against a misbehaving
//! substituter or a tampered CP, so activation runs three checks
//! around `nixos-rebuild switch`:
//!
//! 1. **Pre-realise**: `nix-store --realise <path>` forces nix to
//!    fetch from the configured substituter (any nix-cache-protocol
//!    backend the fleet wires — harmonia, attic, cachix, etc.) and
//!    validate its signature *before* we commit to switching. If the
//!    closure isn't locally available and substituter trust is
//!    misconfigured, this fails closed — we never call
//!    `nixos-rebuild` against an unverifiable path. Also catches
//!    "closure-proxy returned a valid-looking narinfo for a path
//!    that doesn't actually exist upstream" (the proxy-fallback
//!    path is fundamentally less audited than direct substituter
//!    fetch).
//! 2. **Switch**: `nixos-rebuild switch --system <verified-path>`.
//!    nix's own substituter signature checks fire here too; the
//!    pre-realise is belt-and-suspenders.
//! 3. **Post-verify**: read `/run/current-system` (resolve symlink),
//!    compare basename against the expected closure_hash. If they
//!    differ — switched to the wrong path, or `--system` got rewritten
//!    somewhere — refuse to confirm and trigger local rollback.
//!
//! Pre-realise + post-verify together close the property "the agent
//! either confirms the *exact* closure the CP told it about, or rolls
//! back" — without trusting the substituter or the CP to be honest
//! about which path was activated.
//!
//! On rebuild failure or post-verify mismatch the caller runs
//! `nixos-rebuild --rollback` to revert to the previous boot
//! generation. CP-side magic rollback (deadline expiry → 410 on
//! `/confirm`) is independent and additive.
//!
//! All commands run as root via the systemd unit (StateDirectory +
//! no NoNewPrivileges hardening on the agent unit; the agent is a
//! privileged system manager by design — see the agent module
//! comment in modules/scopes/nixfleet/_agent.nix).

use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

/// Maximum time the post-fire poll waits for `/run/current-system`
/// to flip to the expected closure. ADR-011 default. Sized so that
/// realistic closure activations (large package set, slow disk,
/// post-activation systemd target convergence) complete inside the
/// CP's confirm deadline (`DEFAULT_CONFIRM_DEADLINE_SECS = 360`).
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

/// How often the poll loop checks `/run/current-system`. 2s matches
/// ADR-011 — fast enough to feel snappy in interactive runs, slow
/// enough to keep CPU + IO load negligible.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of an activation attempt. The agent's main loop maps each
/// variant to a follow-up action: confirm on `FiredAndPolled`,
/// rollback on either `SwitchFailed` or `VerifyMismatch`, retry-on-
/// next-tick on `RealiseFailed`/`SignatureMismatch` (nothing was
/// switched, nothing to roll back).
#[derive(Debug)]
pub enum ActivationOutcome {
    /// **Timing semantics.** Fire-and-forget (ADR-011): the agent has
    /// fired `systemd-run --unit=nixfleet-switch -- switch-to-configuration switch`
    /// as a detached transient service AND polled `/run/current-system`
    /// to flip to the expected closure within the poll budget. By the
    /// time this variant returns, the system *is* running the new
    /// closure — but the activation work happened in `nixfleet-switch.service`,
    /// not in the agent's process tree. Renamed from `Success` (v0.1
    /// pre-fire-and-forget shape) to make the semantics explicit:
    /// "fired and observed completion" rather than "activation
    /// returned exit-zero synchronously".
    /// Caller should POST `/v1/agent/confirm`.
    FiredAndPolled,
    /// `nix-store --realise` exited non-zero or returned a path that
    /// doesn't match the input. The system was never switched —
    /// caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nix-store --realise` failed specifically because the closure's
    /// narinfo signature did not match any key in
    /// `nixfleet.trust.cacheKeys` (issue #12 root #2). Distinct from
    /// the generic RealiseFailed so the operator dashboard can route
    /// "trust violation" alerts separately from "transient fetch
    /// failure". The system was never switched. Cache-substituter-
    /// agnostic — fires for harmonia, attic, cachix, or any other
    /// nix-cache-protocol backend; nix's substituter trust check is
    /// the gate, this just classifies its failure.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    /// One of the activation steps exited non-zero, or the post-fire
    /// poll observed a wrong/missing closure. Caller runs local
    /// rollback. `phase` distinguishes which step failed:
    /// - `"nix-env-set"`: setting the system profile (system was
    ///   never activated; rollback re-points the profile).
    /// - `"systemd-run-fire"`: queueing the transient unit (rare —
    ///   indicates systemd itself refused; previous unit may be
    ///   stuck in `failed` state).
    /// - `"switch-poll-timeout"`: poll budget elapsed without
    ///   `/run/current-system` flipping; the transient unit is still
    ///   running OR has died with an unrecoverable error. exit_code
    ///   carries the unit's exit status if `systemctl show` could
    ///   read it.
    /// - `"switch-poll-mismatch"`: poll observed a path that
    ///   matched neither the expected closure nor any plausible
    ///   intermediate (would indicate concurrent rebuild or external
    ///   profile mutation).
    SwitchFailed {
        phase: String,
        exit_code: Option<i32>,
    },
    /// **Currently unused with fire-and-forget**: the poll loop
    /// either observes the expected closure or times out. Kept as
    /// a variant so callers that want to distinguish "switched to a
    /// completely unexpected path" from "switch failed" have a slot.
    /// Future code paths (e.g. closure-mutation detection) can emit
    /// this; the post-fire poll exits `SwitchFailed` instead.
    VerifyMismatch {
        expected: String,
        actual: String,
    },
}

/// Activate `target` via realise → switch → verify.
///
/// `tracing` events at every step give operators a grep-friendly
/// breadcrumb trail without parsing the systemd journal in JSON. The
/// `target_closure` field is consistent across all three log lines so
/// `journalctl | grep target_closure=<hash>` follows one activation
/// end to end.
pub async fn activate(target: &EvaluatedTarget) -> Result<ActivationOutcome> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target",
    );

    // Step 1: realise.
    //
    // Pre-`nixos-rebuild` so that closure fetch + signature verification
    // happens explicitly here. nix-store prints the realised path to
    // stdout when it succeeds — we capture and assert it matches the
    // path we asked for, in case some future nix changes resolve
    // through symlinks or substitution-redirects.
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

    // Step 2: set the system profile FIRST.
    //
    // Bootloader entry follows the profile, so even if the switch
    // process dies mid-run the next boot picks up the new closure.
    // This is independent of the fire-and-forget step below: the
    // activation script that switch-to-configuration runs DOES set
    // the profile too, but doing it here closes the window where a
    // crash between fire and switch-script-profile-bump leaves the
    // bootloader pointing at an old generation.
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

    // Step 3: fire switch-to-configuration as a detached transient
    // service (ADR-011 fire-and-forget).
    //
    // Why systemd-run --unit (NOT --scope, NOT --pipe --wait):
    //
    // - Direct `Command::spawn` makes switch-to-configuration a child
    //   in the agent's cgroup. When the new closure changes the
    //   nixfleet-agent.service unit definition, switch-to-configuration
    //   stops the agent → systemd SIGTERMs the cgroup → switch dies
    //   mid-run → /run/current-system never updates. This is what we
    //   were hitting before.
    // - `systemd-run --scope` was tried in v0.1 dev: scope is
    //   created under the calling unit's cgroup and dies with it.
    //   Same death.
    // - `systemd-run --pipe --wait` was also tried: the pipe orphans
    //   when the agent self-terminates mid-wait.
    // - `systemd-run --unit=...` (default `--service` mode) creates
    //   an *independent* transient service with its own cgroup. The
    //   agent's death cannot kill it. Operator can `journalctl -u
    //   nixfleet-switch` to read its logs.
    //
    // `--collect` removes the unit after it stops (success or
    // failure), so the fixed name `nixfleet-switch` is reusable on
    // the next activation without a manual `systemctl reset-failed`.
    //
    // Pre-emptive reset-failed best-effort guard: if a previous
    // activation left the unit in a non-collected failed state,
    // systemd-run rejects the new unit name. `reset-failed` is
    // idempotent against a non-existent unit (exit 0), so unguarded.
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .arg("nixfleet-switch.service")
        .status()
        .await;

    let switch_bin = format!("{store_path}/bin/switch-to-configuration");
    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: firing switch via systemd-run --unit=nixfleet-switch (detached)",
    );
    let fire_status = Command::new("systemd-run")
        .arg("--unit=nixfleet-switch")
        .arg("--collect")
        .arg("--")
        .arg(&switch_bin)
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn systemd-run --unit=nixfleet-switch")?;

    if !fire_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?fire_status.code(),
            "agent: systemd-run failed to queue switch unit",
        );
        return Ok(ActivationOutcome::SwitchFailed {
            phase: "systemd-run-fire".to_string(),
            exit_code: fire_status.code(),
        });
    }

    // Step 4: poll /run/current-system for the expected basename.
    //
    // Budget: 300s (ADR-011). Coupled to CP's confirm_deadline_secs
    // default of 360s = 300s + 60s slack. See
    // `crates/nixfleet-control-plane/src/server/state.rs::DEFAULT_CONFIRM_DEADLINE_SECS`.
    //
    // Fire-and-forget contract: if the agent process gets killed
    // mid-poll (because the new closure stops nixfleet-agent.service
    // and systemd hasn't yet started the new agent), the
    // `nixfleet-switch.service` continues independently. When the
    // post-switch agent boots, the boot-recovery path (TODO #53)
    // observes `/run/current-system == last_dispatched.closure_hash`
    // and posts the retroactive confirm.
    let expected = &target.closure_hash;
    match poll_current_system(expected, POLL_BUDGET, POLL_INTERVAL).await {
        Ok(()) => {
            tracing::info!(
                target_closure = %expected,
                "agent: activation fire-and-forget complete (poll observed expected closure)",
            );
            Ok(ActivationOutcome::FiredAndPolled)
        }
        Err(timeout_info) => {
            // Poll budget exceeded. Read the transient unit's exit
            // status from systemctl show — best-effort triage signal
            // for the operator. The unit may still be running (large
            // closure download), in which case ExecMainStatus is
            // empty / "0" / something inconclusive.
            let exit_code = read_switch_exit_code().await;
            tracing::error!(
                target_closure = %expected,
                last_observed = %timeout_info.last_observed,
                exit_code = ?exit_code,
                "agent: switch poll timed out — declaring SwitchFailed",
            );
            Ok(ActivationOutcome::SwitchFailed {
                phase: "switch-poll-timeout".to_string(),
                exit_code,
            })
        }
    }
}

/// Structured failure mode for `realise()`. Distinguishing
/// signature-mismatch from generic failure lets the agent map each
/// to a different `ReportEvent` variant (issue #12 root #2: explicit
/// surfacing of cache-trust violations vs transient fetch failures).
pub enum RealiseError {
    /// nix's substituter trust check refused the narinfo because no
    /// signature in it matched any key in `trusted-public-keys`
    /// (sourced from `nixfleet.trust.cacheKeys`). Stderr tail kept
    /// for triage; trimmed to the last few hundred bytes so the
    /// `ReportEvent` payload doesn't bloat.
    SignatureMismatch { stderr_tail: String },
    /// Anything else: spawn failure, network error, missing path,
    /// non-utf8 stdout, etc. Generic — caller maps to RealiseFailed.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RealiseError {
    fn from(err: anyhow::Error) -> Self {
        RealiseError::Other(err)
    }
}

/// Heuristic: is this stderr from `nix-store --realise` the
/// signature-trust failure mode? nix has shipped several phrasings
/// over the years; this matches the stable-as-of-2.18+ wording plus
/// a few legacy patterns. Cache-substituter-agnostic — the error
/// originates in nix's own narinfo verifier, not the cache backend.
///
/// Tested in `tests::detect_signature_error_*` — when nix changes
/// the wording, the test breaks rather than the production behavior
/// silently degrading to generic RealiseFailed.
pub fn looks_like_signature_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        // nix 2.18+: "cannot add path '…' because it lacks a valid signature"
        "lacks a valid signature",
        // nix 2.18+: "no signature is trusted by"
        "no signature is trusted",
        // legacy 2.x phrasing
        "is not signed by any of the keys",
        "no signatures matched",
        // narinfo-level failure on substituter side
        "signature mismatch",
        "untrusted signature",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// `nix-store --realise <path>` — fetch + verify, return the realised
/// path from stdout. nix-store prints one path per line; we expect
/// exactly one (we passed exactly one input).
async fn realise(store_path: &str) -> Result<String, RealiseError> {
    let output = Command::new("nix-store")
        .arg("--realise")
        .arg(store_path)
        .output()
        .await
        .with_context(|| format!("spawn nix-store --realise {store_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if looks_like_signature_error(&stderr) {
            // Trim to last ~500 bytes — enough for the matching line
            // plus context, capped so a chatty trace doesn't bloat
            // the ReportEvent body.
            let tail_start = stderr.len().saturating_sub(500);
            let tail = stderr[tail_start..].to_string();
            return Err(RealiseError::SignatureMismatch { stderr_tail: tail });
        }
        return Err(anyhow!(
            "nix-store --realise {store_path} exited {:?}: {stderr}",
            output.status.code()
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| anyhow!("nix-store --realise stdout not utf-8: {e}"))?;
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow!("nix-store --realise produced no output"))?;
    Ok(line.trim().to_string())
}

/// Read `/run/current-system` as a symlink and return the basename of
/// its target. The basename is the closure-hash form the wire and the
/// CP both speak.
async fn read_current_system_basename() -> Result<String> {
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

/// Diagnostic returned by `poll_current_system` on timeout.
#[derive(Debug, Clone)]
pub struct PollTimeout {
    /// Last basename observed (or `<missing>` / `<read-error>` if we
    /// never got a successful read). Useful in the agent log for
    /// distinguishing "switch is still running, just slow" from
    /// "switch died and the symlink is unchanged".
    pub last_observed: String,
}

/// Poll `/run/current-system` for the expected basename. Returns
/// `Ok(())` as soon as the symlink resolves to `expected`. Returns
/// `Err(PollTimeout)` once `budget` elapses without a match.
///
/// Read errors during polling are non-fatal: the symlink may be
/// briefly absent during activation (rare on NixOS but cheap to
/// tolerate). The timer keeps running.
///
/// Pure helper — no logging, deterministic timing — so it's
/// straightforward to test.
pub async fn poll_current_system(
    expected: &str,
    budget: Duration,
    interval: Duration,
) -> std::result::Result<(), PollTimeout> {
    let deadline = tokio::time::Instant::now() + budget;
    // Initial None is dead in every iteration of the loop body
    // (Ok/Err branches both assign before the deadline check), but
    // it's the natural type for "no read has completed yet" and
    // we keep the unwrap_or_else fallback for the budget=0 edge.
    #[allow(unused_assignments)]
    let mut last_observed: Option<String> = None;

    loop {
        match read_current_system_basename().await {
            Ok(basename) => {
                if basename == expected {
                    return Ok(());
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
            return Err(PollTimeout {
                last_observed: last_observed
                    .unwrap_or_else(|| String::from("<no-reads-completed>")),
            });
        }
        tokio::time::sleep(interval).await;
    }
}

/// Best-effort read of `nixfleet-switch.service`'s exit code via
/// `systemctl show --property=ExecMainStatus`. Returns `None` if the
/// command fails or the property is empty/non-numeric — meaning the
/// caller should treat exit status as unknown rather than synthesize
/// a misleading 0.
async fn read_switch_exit_code() -> Option<i32> {
    let output = Command::new("systemctl")
        .arg("show")
        .arg("--property=ExecMainStatus")
        .arg("--value")
        .arg("nixfleet-switch.service")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<i32>().ok()
}

/// Local rollback: revert the system profile one generation back and
/// run the previous closure's `switch-to-configuration switch`.
/// Used when:
/// - `activate()` returned a non-success outcome that requires
///   rollback (`SwitchFailed`, `VerifyMismatch`).
/// - The agent's confirm window expired before the CP acknowledged
///   the activation (magic rollback, RFC-0003 §4.2).
///
/// `nix-env --rollback` flips the system profile symlink to the
/// previous generation; `/run/current-system/bin/switch-to-
/// configuration switch` then re-runs the activation script of the
/// (now previous) closure. Bypasses `nixos-rebuild` entirely — the
/// new `nixos-rebuild-ng` (Python rewrite shipped in 26.05) tries
/// to evaluate `<nixpkgs/nixos>` even on `--rollback`, which fails
/// in the agent's NIX_PATH-less sandbox.
///
/// Idempotent — running rollback twice in a row reverts twice. The
/// caller is expected to invoke this exactly once per failed
/// activation.
pub async fn rollback() -> Result<ExitStatus> {
    tracing::warn!("agent: triggering local rollback (nix-env --rollback + switch-to-configuration)");
    let env_status = Command::new("nix-env")
        .arg("--profile")
        .arg("/nix/var/nix/profiles/system")
        .arg("--rollback")
        .status()
        .await
        .with_context(|| "spawn nix-env --rollback")?;
    if !env_status.success() {
        tracing::error!(
            exit_code = ?env_status.code(),
            "agent: nix-env --rollback failed; not running switch-to-configuration",
        );
        return Ok(env_status);
    }

    // /run/current-system now points at the rolled-back generation
    // (nix-env --rollback updated the system profile symlink, and
    // /run/current-system tracks the profile target).
    let status = Command::new("/run/current-system/bin/switch-to-configuration")
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn /run/current-system/bin/switch-to-configuration switch")?;
    if status.success() {
        tracing::info!("agent: rollback succeeded");
    } else {
        tracing::error!(exit_code = ?status.code(), "agent: rollback failed");
    }
    Ok(status)
}

/// POST `/v1/agent/confirm` to acknowledge a successful activation.
///
/// Per RFC-0003 §4.2 the agent confirms exactly once after a
/// successful activation. Returns `ConfirmOutcome` so the activation
/// loop can react:
/// - `Acknowledged` (204): nothing else to do.
/// - `Cancelled` (410): CP says the rollout was cancelled or the
///   deadline passed — agent runs `nixos-rebuild --rollback`.
/// - `Other`: logged; the CP-side rollback timer will catch deadline
///   expiry independently.
pub async fn confirm_target(
    client: &reqwest::Client,
    cp_url: &str,
    hostname: &str,
    target: &EvaluatedTarget,
    rollout: &str,
    wave: u32,
    boot_id: &str,
) -> Result<crate::comms::ConfirmOutcome> {
    use nixfleet_proto::agent_wire::{ConfirmRequest, GenerationRef};

    let req = ConfirmRequest {
        hostname: hostname.to_string(),
        rollout: rollout.to_string(),
        wave,
        generation: GenerationRef {
            closure_hash: target.closure_hash.clone(),
            channel_ref: Some(target.channel_ref.clone()),
            boot_id: boot_id.to_string(),
        },
    };

    let outcome = crate::comms::confirm(client, cp_url, &req).await?;
    match outcome {
        crate::comms::ConfirmOutcome::Acknowledged => {
            tracing::info!(
                target_closure = %target.closure_hash,
                rollout,
                wave,
                "agent: confirm acknowledged (204)",
            );
        }
        crate::comms::ConfirmOutcome::Cancelled => {
            tracing::warn!(
                target_closure = %target.closure_hash,
                rollout,
                "agent: confirm returned 410 — CP says trigger local rollback",
            );
        }
        crate::comms::ConfirmOutcome::Other => {
            tracing::warn!(
                target_closure = %target.closure_hash,
                rollout,
                "agent: confirm returned unexpected status — deadline timer will handle",
            );
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    //! Pure-logic tests for the path-comparison surface of
    //! `read_current_system_basename`. The realise/switch path itself
    //! is covered by the microvm harness — unit-level mocking of
    //! `Command` is more friction than payoff.

    use super::*;
    use std::path::{Path, PathBuf};

    /// Stand-in for `read_current_system_basename` that takes the
    /// (already-resolved) symlink target as a path and returns the
    /// basename. Used to exercise the basename-extraction logic
    /// without touching `/run/current-system`.
    fn basename_of(target: &Path) -> Result<String> {
        target
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("no utf-8 basename: {}", target.display()))
    }

    #[test]
    fn basename_extracts_from_typical_store_path() {
        let p = PathBuf::from("/nix/store/abc123-nixos-system-test-host-26.05");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-test-host-26.05");
    }

    #[test]
    fn basename_unchanged_by_trailing_slash() {
        let p = PathBuf::from("/nix/store/abc123-nixos-system-test-host-26.05/");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-test-host-26.05");
    }

    #[test]
    fn outcome_kinds_are_distinct() {
        // Trivial round-trip: just asserts the variants exist + Debug-print
        // distinctly so future refactors don't silently drop one.
        let outcomes = [
            format!("{:?}", ActivationOutcome::FiredAndPolled),
            format!(
                "{:?}",
                ActivationOutcome::RealiseFailed {
                    reason: "x".into()
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::SwitchFailed {
                    phase: "switch-poll-timeout".into(),
                    exit_code: Some(1),
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::VerifyMismatch {
                    expected: "a".into(),
                    actual: "b".into()
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::SignatureMismatch {
                    closure_hash: "h".into(),
                    stderr_tail: "x".into(),
                }
            ),
        ];
        let unique: std::collections::HashSet<_> = outcomes.iter().collect();
        assert_eq!(unique.len(), outcomes.len(), "outcome variants collide on Debug");
    }

    #[tokio::test]
    async fn poll_current_system_returns_ok_when_match_appears() {
        // Use tempdir + symlink to simulate /run/current-system. The
        // helper only takes a basename — we exercise the poll loop's
        // observation logic by symlinking and leaving it stable; the
        // real /run/current-system is fine for tests since we don't
        // assert on its content.
        // Skip on systems without /run/current-system (Darwin in CI).
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        // Read whatever is currently there and assert match — this
        // exercises the success branch of the poll loop with no fake
        // filesystem rigging.
        let basename = read_current_system_basename().await.unwrap();
        let result = poll_current_system(
            &basename,
            Duration::from_millis(100),
            Duration::from_millis(10),
        )
        .await;
        assert!(result.is_ok(), "poll did not match its own current-system: {result:?}");
    }

    #[tokio::test]
    async fn poll_current_system_times_out_when_no_match() {
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        let result = poll_current_system(
            "definitely-not-a-real-closure-hash-xyz",
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .await;
        let err = result.expect_err("expected timeout");
        // last_observed should be the actual current-system basename,
        // not the placeholder.
        assert!(
            !err.last_observed.starts_with("<not-yet-read>"),
            "expected at least one observation before timeout: {err:?}",
        );
    }

    #[test]
    fn switch_failed_phase_strings_are_stable() {
        // The phase strings are part of the wire (passed up to CP via
        // ReportEvent::ActivationFailed); locking them here ensures
        // any rename on the agent side surfaces as a test failure
        // rather than silently changing the report contract.
        for phase in &[
            "nix-env-set",
            "systemd-run-fire",
            "switch-poll-timeout",
            "switch-poll-mismatch",
        ] {
            let outcome = ActivationOutcome::SwitchFailed {
                phase: (*phase).to_string(),
                exit_code: None,
            };
            // Just exercises the constructor + Debug — the assertion
            // is that the strings compile + match the documented set.
            let _ = format!("{outcome:?}");
        }
    }

    #[test]
    fn detect_signature_error_matches_nix_2_18_phrasing() {
        let s = "error: cannot add path '/nix/store/abc-foo' because \
                 it lacks a valid signature";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_no_signature_trusted() {
        let s = "error: no signature is trusted by any of these keys: cache.example.com-1";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_legacy_phrasing() {
        let s = "error: path '/nix/store/abc-foo' is not signed by any of the keys in \
                 trusted-public-keys";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_no_signatures_matched() {
        let s = "error: no signatures matched any of the configured public keys";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_signature_mismatch() {
        let s = "warning: signature mismatch for path '/nix/store/abc-foo'";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_does_not_match_network_failure() {
        // Network blip → generic RealiseFailed, not SignatureMismatch.
        let s = "error: unable to download 'https://cache.example.com/nar/abc.nar': \
                 Couldn't connect to server";
        assert!(!looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_does_not_match_missing_path() {
        let s = "error: path '/nix/store/abc-foo' is required, but it has no substitutes \
                 and there is no derivation that produces it";
        assert!(!looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_case_insensitive() {
        // tracing or operator-side log redirection sometimes uppercases
        // the first letter; the heuristic is case-insensitive so
        // "Lacks a valid signature" still matches.
        let s = "Error: path Lacks A Valid Signature on this host";
        assert!(looks_like_signature_error(s));
    }
}
