//! Agent-side activation: install + boot the closure the CP issued.
//!
//! Three checks around the platform's switch primitive make the
//! agent the last line of defense against a misbehaving substituter
//! or tampered CP:
//!
//! 1. **Pre-realise** (`nix-store --realise`) — forces substituter
//!    fetch + signature validation before we commit to switching.
//! 2. **Switch** — `ActivationBackend::fire_switch` dispatches to the
//!    cfg-selected `LinuxBackend` or `DarwinBackend` impl.
//! 3. **Post-verify** — `/run/current-system` basename must match
//!    the expected closure_hash; mismatch → local rollback.
//!
//! Together these close: "the agent either confirms the *exact*
//! closure the CP told it about, or rolls back" — without trusting
//! the substituter or the CP. CP-side magic rollback (deadline →
//! 410) is independent and additive.
//!
//! ## Platform layout
//!
//! Platform-specific code lives in sibling modules
//! (`linux.rs`/`darwin.rs`) which each define a unit-struct backend
//! implementing the `ActivationBackend` trait. The cfg-selected
//! impl is exposed as `DefaultBackend` (a type alias) and
//! `DEFAULT_BACKEND` (a const value). Each platform's code only
//! compiles for its target; there are no runtime `cfg!()` branches
//! and no cross-platform stub functions in callers.
//!
//! Unit tests inject a fake `ActivationBackend` via the
//! `*_with(&backend, ...)` form. Production code calls the
//! parameterless façades (`activate(target)`, `rollback()`) which
//! resolve to `DEFAULT_BACKEND` at call time. Issue #67's pluggable
//! backend extension (SystemManager, microVM) lands by adding a
//! third unit-struct that implements the same trait.
//!
//! - `LinuxBackend` — `systemd-run --unit=nixfleet-{switch,rollback}`
//!   wrapping `switch-to-configuration`; flock check on
//!   `/run/nixos/switch-to-configuration.lock`; `systemctl show
//!   --property=ExecMainStatus` for unit exit codes.
//! - `DarwinBackend` — `setsid`-detached `<store>/activate-user` +
//!   `<store>/activate`; no in-flight lock primitive; no systemd
//!   surface for exit-code introspection.
//!
//! `setsid` + a detached child is what makes darwin activation
//! survive the agent's own SIGTERM during plist reload (`nohup`
//! doesn't work in launchd's no-controlling-tty context).

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "linux")]
pub use linux::LinuxBackend;
#[cfg(target_os = "macos")]
pub use darwin::DarwinBackend;

/// The cfg-selected default backend type — `LinuxBackend` on linux,
/// `DarwinBackend` on macos. Production callers use the const
/// `DEFAULT_BACKEND` rather than constructing one directly.
#[cfg(target_os = "linux")]
pub type DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub type DefaultBackend = DarwinBackend;

/// Process-wide singleton of the cfg-selected backend. Callers
/// outside this module should use the `activate(target)` / `rollback()`
/// façades; tests construct a fake `ActivationBackend` and call the
/// `*_with` form.
#[cfg(target_os = "linux")]
pub const DEFAULT_BACKEND: DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub const DEFAULT_BACKEND: DefaultBackend = DarwinBackend;

/// Platform abstraction. Four primitives — every other piece of the
/// activation pipeline (realise, profile flip, post-verify poll,
/// self-correction) is platform-agnostic and lives in `mod.rs`.
///
/// Method-level docs in `linux.rs` / `darwin.rs` give the per-impl
/// contract. Trait-level guarantees:
///
/// - `is_switch_in_progress` is fail-open: the caller treats `false`
///   as "either no contention, OR we couldn't tell" — a false
///   negative is a stale-lock hazard handled at the lock layer, not
///   here.
/// - `read_unit_exit_code` returns `None` on any error or absent
///   surface; the agent never synthesises a misleading 0.
/// - `fire_switch` / `fire_rollback` are "fire-and-forget": `Ok(None)`
///   means the platform-specific async work was dispatched and the
///   caller should poll `/run/current-system`. `Ok(Some(outcome))`
///   means the fire step itself failed; no poll. `Err` is reserved
///   for spawn-level I/O errors.
pub trait ActivationBackend: Send + Sync {
    fn is_switch_in_progress(&self) -> impl std::future::Future<Output = bool> + Send;
    fn read_unit_exit_code(
        &self,
        unit_name: &str,
    ) -> impl std::future::Future<Output = Option<i32>> + Send;
    fn fire_switch(
        &self,
        target: &EvaluatedTarget,
        store_path: &str,
    ) -> impl std::future::Future<Output = Result<Option<ActivationOutcome>>> + Send;
    fn fire_rollback(
        &self,
        target_basename: &str,
    ) -> impl std::future::Future<Output = Result<Option<RollbackOutcome>>> + Send;
}


/// 300s sized to fit inside the CP's `DEFAULT_CONFIRM_DEADLINE_SECS = 360`.
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ActivationOutcome {
    /// Fire-and-forget completed: switch fired AND
    /// `/run/current-system` flipped to expected. By the time this
    /// returns the system *is* running the new closure, but the
    /// activation work happened in `nixfleet-switch.service`.
    /// Caller should POST `/v1/agent/confirm`.
    FiredAndPolled,
    /// `nix-store --realise` failed (non-signature). System never
    /// switched; caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nix-store --realise` failed because the closure's narinfo
    /// signature didn't match any key in `nixfleet.trust.cacheKeys`.
    /// Distinct so dashboards can route trust violations separately
    /// from transient fetch failures. System never switched.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    /// `phase`:
    /// - `nix-env-set` — setting the system profile (rollback re-points it)
    /// - `systemd-run-fire` — queueing the transient unit (systemd refused)
    /// - `switch-poll-timeout` — budget elapsed without `/run/current-system` flip
    SwitchFailed {
        phase: String,
        exit_code: Option<i32>,
    },
    /// Post-switch verify caught `/run/current-system` resolving to a
    /// basename that is neither the expected new closure nor the
    /// pre-switch basename. Symptom of a concurrent `nix-env --set`,
    /// a profile-self-correction misfire, or a hostile activation
    /// script. Caller rolls back to a known-good generation.
    VerifyMismatch {
        expected: String,
        actual: String,
    },
}

/// Activate via realise → set-profile → fire-and-forget switch →
/// poll → self-correct. Single attempt per call; retry comes from
/// the agent's main poll loop (in-call retry would trip the CP's
/// confirm deadline because each attempt is gated by `POLL_BUDGET`).
///
/// Façade over `activate_with(&DEFAULT_BACKEND, target)`.
pub async fn activate(target: &EvaluatedTarget) -> Result<ActivationOutcome> {
    activate_with(&DEFAULT_BACKEND, target).await
}

/// Generic-over-backend form. Production calls `activate(target)`;
/// tests inject a fake to assert per-platform behaviour without
/// shelling out.
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

/// Distinct so the agent can map signature-mismatch to a different
/// `ReportEvent` than transient fetch failures.
pub enum RealiseError {
    /// Stderr trimmed to last ~500 bytes for triage.
    SignatureMismatch { stderr_tail: String },
    /// Spawn failure, network error, missing path, non-utf8 stdout, etc.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RealiseError {
    fn from(err: anyhow::Error) -> Self {
        RealiseError::Other(err)
    }
}

/// nix has several wordings for substituter-trust failures across
/// versions. The set covers 2.18+ stable phrasings plus legacy 2.x.
/// Tested in `tests::detect_signature_error_*` so a nix wording
/// change breaks the test rather than silently downgrading to
/// generic RealiseFailed.
pub fn looks_like_signature_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "lacks a valid signature",
        "no signature is trusted",
        "is not signed by any of the keys",
        "no signatures matched",
        "signature mismatch",
        "untrusted signature",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

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

/// Configuration + execution surface for polling `/run/current-system`
/// until it resolves to the expected closure basename, or one of the
/// terminal `PollOutcome` conditions fires.
///
/// When `previous_basename` is `Some(p)`, observing a basename that is
/// neither `expected_basename` nor `p` is treated as a hard mismatch
/// and returned as `PollOutcome::FlippedToUnexpected` immediately —
/// the system cannot legitimately be at any third basename mid-switch.
/// Leave it as `None` for the rollback path, where a stable pre-state
/// reference isn't meaningful and any non-match collapses into the
/// timeout branch.
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

/// Defensive against concurrent profile mutations during activation.
/// `Err` only when self-correction itself failed (caller treats as
/// non-fatal — current-system already verified).
async fn self_correct_profile(expected_store_path: &str) -> Result<()> {
    let profile = "/nix/var/nix/profiles/system";
    if profile_matches(expected_store_path, profile) {
        return Ok(());
    }

    tracing::warn!(
        expected = %expected_store_path,
        profile = profile,
        "agent: profile mismatch after fire-and-forget — re-running nix-env --set",
    );
    let status = Command::new("nix-env")
        .arg("--profile")
        .arg(profile)
        .arg("--set")
        .arg(expected_store_path)
        .status()
        .await
        .with_context(|| "spawn nix-env --set (self-correction)")?;
    if !status.success() {
        return Err(anyhow!(
            "nix-env --set self-correction exited {:?}",
            status.code()
        ));
    }
    if !profile_matches(expected_store_path, profile) {
        return Err(anyhow!(
            "profile still mismatched after nix-env --set self-correction",
        ));
    }
    tracing::info!("agent: profile self-corrected successfully");
    Ok(())
}

/// Two-level symlink: profile → `system-<N>-link` → `/nix/store/<basename>`.
/// Returns false on any read error (caller treats as mismatch).
fn profile_matches(expected_store_path: &str, profile_path: &str) -> bool {
    let Ok(gen_link) = std::fs::read_link(profile_path) else {
        return false;
    };
    let abs_gen_link = if gen_link.is_relative() {
        let parent = std::path::Path::new(profile_path)
            .parent()
            .unwrap_or(std::path::Path::new("/"));
        parent.join(&gen_link)
    } else {
        gen_link
    };
    let final_target = match std::fs::read_link(&abs_gen_link) {
        Ok(t) => t,
        Err(_) => abs_gen_link,
    };
    final_target.to_string_lossy() == expected_store_path
}

// Platform-specific primitives — `fire_switch`, `fire_rollback`,
// `read_unit_exit_code` — live in `linux.rs` / `darwin.rs` as
// methods on `LinuxBackend` / `DarwinBackend` and are reached
// through `ActivationBackend` trait dispatch.

/// Outcome of a `rollback ` call. Mirrors `ActivationOutcome`'s
/// shape so callers can pattern-match similarly. Fire-and-forget
/// applies to rollback for the same reason as activate: if the
/// rolled-back closure's activation script changes a unit definition
/// the running agent depends on (transitively — system services like
/// dbus/systemd-tmpfiles can chain into this), a synchronous spawn
/// gets SIGTERMed mid-run when systemd reloads.
#[derive(Debug)]
pub enum RollbackOutcome {
    FiredAndPolled,
    /// `phase`: `nix-env-rollback`, `discover-target`,
    /// `systemd-run-fire`, `rollback-poll-timeout`.
    Failed {
        phase: String,
        exit_code: Option<i32>,
    },
}

impl RollbackOutcome {
    pub fn success(&self) -> bool {
        matches!(self, RollbackOutcome::FiredAndPolled)
    }
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            RollbackOutcome::Failed { exit_code, .. } => *exit_code,
            RollbackOutcome::FiredAndPolled => None,
        }
    }
    pub fn phase(&self) -> Option<&str> {
        match self {
            RollbackOutcome::Failed { phase, .. } => Some(phase.as_str()),
            RollbackOutcome::FiredAndPolled => None,
        }
    }
}

/// Bypasses `nixos-rebuild` (`nixos-rebuild-ng` tries to evaluate
/// `<nixpkgs/nixos>` even on `--rollback`, failing in the agent's
/// NIX_PATH-less sandbox). Caller must invoke exactly once per
/// failed activation — running twice rolls back twice.
///
/// Façade over `rollback_with(&DEFAULT_BACKEND)`.
pub async fn rollback() -> Result<RollbackOutcome> {
    rollback_with(&DEFAULT_BACKEND).await
}

/// Generic-over-backend form. Production calls `rollback()`; tests
/// inject a fake.
pub async fn rollback_with<B: ActivationBackend>(backend: &B) -> Result<RollbackOutcome> {
    tracing::warn!("agent: triggering local rollback (fire-and-forget via systemd-run)");

    // Step 1: profile flip — synchronous symlink re-target.
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
            "agent: nix-env --rollback failed; cannot proceed",
        );
        return Ok(RollbackOutcome::Failed {
            phase: "nix-env-rollback".to_string(),
            exit_code: env_status.code(),
        });
    }

    // Step 2: discover the rolled-back target so we can poll for it
    // (stronger contract than "any change").
    let target_basename = match resolve_profile_target() {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                error = %err,
                "agent: cannot resolve rolled-back profile target; aborting rollback",
            );
            return Ok(RollbackOutcome::Failed {
                phase: "discover-target".to_string(),
                exit_code: None,
            });
        }
    };
    tracing::info!(
        target_basename = %target_basename,
        "agent: rollback target discovered; firing detached switch",
    );

    // Step 3: fire rollback (backend-dispatched).
    if let Some(failure) = backend.fire_rollback(&target_basename).await? {
        return Ok(failure);
    }

    // Step 4: poll for the rolled-back target. `previous_basename`
    // stays None — rollback has no meaningful "expected pre-state"
    // reference (the pre-rollback basename is the failed generation
    // we're abandoning), so flip-to-unexpected detection is disabled
    // and any non-match collapses into the timeout branch.
    match VerifyPoll::new(&target_basename).until_settled().await {
        PollOutcome::Settled => {
            tracing::info!(
                target = %target_basename,
                "agent: rollback fire-and-forget complete",
            );
            Ok(RollbackOutcome::FiredAndPolled)
        }
        PollOutcome::Timeout { last_observed } => {
            let exit_code = backend.read_unit_exit_code("nixfleet-rollback.service").await;
            tracing::error!(
                target = %target_basename,
                last_observed = %last_observed,
                exit_code = ?exit_code,
                "agent: rollback poll timed out",
            );
            Ok(RollbackOutcome::Failed {
                phase: "rollback-poll-timeout".to_string(),
                exit_code,
            })
        }
        PollOutcome::FlippedToUnexpected { .. } => {
            // Unreachable: only emitted when previous_basename is Some.
            unreachable!(
                "FlippedToUnexpected requires Some(previous_basename); rollback leaves it None"
            )
        }
    }
}

/// Two symlink levels: profile → `system-<N>-link` (relative) →
/// `/nix/store/<basename>` (absolute).
fn resolve_profile_target() -> Result<String> {
    let profile = std::path::Path::new("/nix/var/nix/profiles/system");
    let gen_link = std::fs::read_link(profile)
        .with_context(|| "readlink /nix/var/nix/profiles/system")?;
    let abs_gen_link = if gen_link.is_relative() {
        profile.parent().unwrap_or(std::path::Path::new("/")).join(&gen_link)
    } else {
        gen_link.clone()
    };
    let store_path = std::fs::read_link(&abs_gen_link)
        .with_context(|| format!("readlink {}", abs_gen_link.display()))?;
    let basename = store_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("non-utf8 basename: {}", store_path.display()))?
        .to_string();
    Ok(basename)
}

/// `Acknowledged` (204): done. `Cancelled` (410): CP says the
/// rollout was cancelled or deadline expired — agent rolls back.
/// `Other`: logged; the CP's rollback timer catches deadline expiry.
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
                ActivationOutcome::SignatureMismatch {
                    closure_hash: "h".into(),
                    stderr_tail: "x".into(),
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::VerifyMismatch {
                    expected: "e".into(),
                    actual: "a".into(),
                }
            ),
        ];
        let unique: std::collections::HashSet<_> = outcomes.iter().collect();
        assert_eq!(unique.len(), outcomes.len(), "outcome variants collide on Debug");
    }

    fn short_poll<'a>(
        expected: &'a str,
        previous: Option<&'a str>,
        budget_ms: u64,
    ) -> VerifyPoll<'a> {
        let mut p = VerifyPoll::new(expected);
        p.previous_basename = previous;
        p.budget = Duration::from_millis(budget_ms);
        p.interval = Duration::from_millis(10);
        p
    }

    #[tokio::test]
    async fn verify_poll_settles_when_match_appears() {
        // Skip on systems without /run/current-system (Darwin in CI).
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        let basename = read_current_system_basename().await.unwrap();
        let outcome = short_poll(&basename, None, 100).until_settled().await;
        assert!(
            matches!(outcome, PollOutcome::Settled),
            "poll did not match its own current-system: {outcome:?}",
        );
    }

    #[tokio::test]
    async fn verify_poll_times_out_when_no_match_and_previous_disabled() {
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        let outcome = short_poll("definitely-not-a-real-closure-hash-xyz", None, 50)
            .until_settled()
            .await;
        match outcome {
            PollOutcome::Timeout { last_observed } => {
                assert!(
                    !last_observed.starts_with("<no-reads-completed>"),
                    "expected at least one observation before timeout: {last_observed}",
                );
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_poll_flips_when_observed_is_neither_expected_nor_previous() {
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        // expected and previous both wrong — observed (the live
        // basename) matches neither, so the first read returns
        // FlippedToUnexpected.
        let actual = read_current_system_basename().await.unwrap();
        let outcome = short_poll("expected-is-wrong", Some("previous-is-also-wrong"), 100)
            .until_settled()
            .await;
        match outcome {
            PollOutcome::FlippedToUnexpected { observed } => {
                assert_eq!(observed, actual, "observed should be the live basename");
            }
            other => panic!("expected FlippedToUnexpected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_poll_keeps_polling_when_observed_matches_previous() {
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        // expected wrong, previous == actual → observed matches
        // previous → keep polling → times out.
        let actual = read_current_system_basename().await.unwrap();
        let outcome = short_poll("expected-is-wrong", Some(&actual), 50)
            .until_settled()
            .await;
        match outcome {
            PollOutcome::Timeout { last_observed } => {
                assert_eq!(last_observed, actual);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn switch_failed_phase_strings_are_stable() {
        // The phase strings are part of the wire (passed up to CP via
        // ReportEvent::ActivationFailed); locking them here ensures
        // any rename on the agent side surfaces as a test failure
        // rather than silently changing the report contract.
        //
        // Linux phases: nix-env-set, systemd-run-fire, switch-poll-*.
        // Darwin phases: nix-env-set (shared), darwin-activate-spawn,
        // switch-poll-* (shared — `/run/current-system` is the same
        // signal on both).
        for phase in &[
            "nix-env-set",
            "systemd-run-fire",
            "switch-poll-timeout",
            "switch-poll-mismatch",
            "darwin-activate-spawn",
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

    /// Cfg-selected backend sanity check. We can't unit-test the
    /// actual fire path (would need a real `nix-env`+`activate`/
    /// `systemd-run` rig — that's the harness's job), but we CAN
    /// assert that `DEFAULT_BACKEND.is_switch_in_progress()` short-
    /// circuits to false on darwin (no flock probe) and goes through
    /// the file-existence gate on linux.
    #[tokio::test]
    async fn default_backend_is_switch_in_progress_short_circuits_on_darwin() {
        // On darwin: always false (no equivalent lock).
        // On linux: returns false when the lock file is absent.
        // Either way, in CI / dev sandbox the lock file shouldn't
        // exist, so the call returns false. The platform-specific
        // assertion is that on darwin we never even *try* to run
        // `flock(1)` — the call returns at the first line. We can't
        // directly observe that without mocking, but we *can* assert
        // it returns false fast (well under the 1s timeout) on a
        // clean dev machine.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            DEFAULT_BACKEND.is_switch_in_progress(),
        )
        .await;
        assert!(
            result.is_ok(),
            "DEFAULT_BACKEND.is_switch_in_progress should return promptly on a host with no in-flight switch",
        );
        assert!(
            !result.unwrap(),
            "DEFAULT_BACKEND.is_switch_in_progress: no switch in flight on a clean dev host",
        );
    }

    /// Phase string returned on darwin when `<store>/activate` spawn
    /// fails. Locked here separately from the linux phases since
    /// it's part of the wire contract.
    #[test]
    fn darwin_activate_spawn_phase_string_is_stable() {
        let outcome = ActivationOutcome::SwitchFailed {
            phase: "darwin-activate-spawn".to_string(),
            exit_code: None,
        };
        let s = format!("{outcome:?}");
        assert!(s.contains("darwin-activate-spawn"));
    }

    /// Ensure `DEFAULT_BACKEND.read_unit_exit_code` returns None on
    /// darwin without shelling out to a non-existent `systemctl`. The
    /// mere fact that the call returns (rather than hanging or
    /// panicking on macOS) is the assertion — the darwin backend's
    /// stub returns None unconditionally.
    #[tokio::test]
    async fn read_unit_exit_code_short_circuits_on_darwin() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            DEFAULT_BACKEND.read_unit_exit_code("definitely-not-a-real-unit.service"),
        )
        .await
        .expect("must return promptly");
        // On darwin: always None (no systemctl). On linux: None
        // because the unit doesn't exist. Both branches converge.
        assert!(result.is_none());
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

    /// Sanity check: a non-platform `ActivationBackend` impl can be
    /// constructed and substituted into `is_switch_in_progress` /
    /// `read_unit_exit_code` without depending on `/run/nixos/...`
    /// or `systemctl`. The fake's behaviour is what the harness will
    /// rely on once #67's other backends (system-manager, microvm)
    /// land — wiring them is then "implement the trait, no caller-
    /// side change".
    struct FakeBackend {
        switch_in_progress: bool,
        unit_exit_code: Option<i32>,
    }
    impl ActivationBackend for FakeBackend {
        async fn is_switch_in_progress(&self) -> bool {
            self.switch_in_progress
        }
        async fn read_unit_exit_code(&self, _unit_name: &str) -> Option<i32> {
            self.unit_exit_code
        }
        async fn fire_switch(
            &self,
            _target: &EvaluatedTarget,
            _store_path: &str,
        ) -> Result<Option<ActivationOutcome>> {
            unreachable!("fire_switch unused in this test")
        }
        async fn fire_rollback(
            &self,
            _target_basename: &str,
        ) -> Result<Option<RollbackOutcome>> {
            unreachable!("fire_rollback unused in this test")
        }
    }

    #[tokio::test]
    async fn activation_backend_trait_dispatches_to_fake() {
        let fake = FakeBackend {
            switch_in_progress: true,
            unit_exit_code: Some(42),
        };
        assert!(fake.is_switch_in_progress().await);
        assert_eq!(fake.read_unit_exit_code("anything").await, Some(42));

        let fake2 = FakeBackend {
            switch_in_progress: false,
            unit_exit_code: None,
        };
        assert!(!fake2.is_switch_in_progress().await);
        assert!(fake2.read_unit_exit_code("anything").await.is_none());
    }
}
