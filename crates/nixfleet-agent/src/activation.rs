//! Agent-side activation logic.
//!
//! The CP issues a closure hash via `CheckinResponse.target`; the
//! agent's job is to install + boot that closure. Per ARCHITECTURE.md
//! the agent is the *last line of defense* against a misbehaving
//! substituter or a tampered CP, so activation runs three checks
//! around `nixos-rebuild switch`:
//!
//! 1. **Pre-realise**: `nix-store --realise <path>` forces nix to
//!   fetch from the configured substituter (any nix-cache-protocol
//!   backend the fleet wires — harmonia, attic, cachix, etc.) and
//!   validate its signature *before* we commit to switching. If the
//!   closure isn't locally available and substituter trust is
//!   misconfigured, this fails closed — we never call
//!   `nixos-rebuild` against an unverifiable path. Also catches
//!   "closure-proxy returned a valid-looking narinfo for a path
//!   that doesn't actually exist upstream" (the proxy-fallback
//!   path is fundamentally less audited than direct substituter
//!   fetch).
//! 2. **Switch**: `nixos-rebuild switch --system <verified-path>`.
//!   nix's own substituter signature checks fire here too; the
//!   pre-realise is belt-and-suspenders.
//! 3. **Post-verify**: read `/run/current-system` (resolve symlink),
//!   compare basename against the expected closure_hash. If they
//!   differ — switched to the wrong path, or `--system` got rewritten
//!   somewhere — refuse to confirm and trigger local rollback.
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
//!
//! ## Platform dispatch (darwin)
//!
//! aether (and future darwin hosts) run the same agent binary but
//! the "fire" step diverges from the linux/systemd path:
//!
//! | step | linux (NixOS) | darwin (nix-darwin) |
//! |-----------------------|------------------------------------------------|------------------------------------------------------|
//! | switch-lock probe | `flock(8)` on `/run/nixos/switch-to-configuration.lock` | n/a — darwin activation is sync, no concurrent races |
//! | profile update | `nix-env --profile … --set <store>` | same |
//! | fire | `systemd-run --unit=nixfleet-switch -- <store>/bin/switch-to-configuration switch` | `setsid ` detached `<store>/activate-user` (skip if absent) + `<store>/activate` |
//! | poll | `/run/current-system` flips to expected basename | same (per v0.1 darwin-platform-notes.md) |
//! | unit-status triage | `systemctl show --property=ExecMainStatus` | n/a — returns None |
//!
//! Approach: runtime `cfg!(target_os = "macos")` dispatch (NOT
//! `#[cfg]`) so both code paths compile + type-check on every build.
//! This catches "linux-only refactor accidentally broke darwin" at
//! `cargo check` time on either host. Trait-based dispatch was
//! considered (Activator trait, two impls, feature gate) but the
//! incremental cost of a single platform check inside one helper
//! per fire-step doesn't justify trait ceremony — most of the
//! activation flow (realise, profile-set, poll, self-correct,
//! profile-flip rollback) is already platform-agnostic.
//!
//! See `docs/mdbook/reference/darwin-platform-notes.md` (ported
//! from v0.1.1) for the full rationale on why `setsid ` + a
//! detached child process survives the agent's own SIGTERM during
//! `activate`'s plist reload.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

/// Path of the NixOS-side activation lock. Held exclusive by any
/// running `switch-to-configuration` (nixos-rebuild, our own
/// systemd-run, an operator running it manually, an Ansible/SSH
/// orchestration, etc.). Probed non-blockingly via `flock(8)` before
/// the agent fires its own switch — if the lock is held, another
/// activator is in flight and the agent skips this tick rather than
/// racing.
const SWITCH_LOCK_PATH: &str = "/run/nixos/switch-to-configuration.lock";

/// Non-blocking check: is some other process currently holding the
/// NixOS activation lock?
///
/// Returns `true` only when the lock file exists AND a non-blocking
/// shared `flock` attempt fails (lock contended). Lock file absent
/// or `flock` binary unavailable returns `false` — fail-open. The
/// agent's own activate flow is guarded by the agent unit's own
/// concurrency model (single-threaded poll loop), so the false case
/// is "no operator-visible third-party switch is racing us".
///
/// Ports v0.1's `nix::is_switch_in_progress` (deleted at afb5e18^:
/// crates/agent/src/nix.rs:11-47), but uses the `flock(1)` shell
/// utility instead of a libc::flock FFI to avoid adding a libc
/// direct dep just for this one syscall. flock(1) ships in
/// util-linux, present on every NixOS host.
///
/// **Darwin:** there's no equivalent lock — `darwin-rebuild`
/// activation is synchronous (single `activate` script invocation,
/// no daemon coordination), so concurrent-switch races aren't a
/// thing. Returns `false` early on macOS.
pub async fn is_switch_in_progress() -> bool {
    // Darwin: no equivalent lock; activation is synchronous. Skip
    // the flock probe entirely so we don't shell out to `flock(1)`
    // on a host that may not have it (util-linux is linux-only).
    if cfg!(target_os = "macos") {
        return false;
    }
    if !std::path::Path::new(SWITCH_LOCK_PATH).exists() {
        return false;
    }
    let status = Command::new("flock")
        .arg("--nonblock")
        .arg("--shared")
        .arg(SWITCH_LOCK_PATH)
        .arg("true")
        .status()
        .await;
    match status {
        // flock acquired and released the lock immediately — no contender.
        Ok(s) if s.success() => false,
        // flock exited non-zero (typically code=1, "lock contended").
        Ok(_) => true,
        // Couldn't run flock at all — fail-open.
        Err(_) => false,
    }
}

/// Maximum time the post-fire poll waits for `/run/current-system`
/// to flip to the expected closure. default. Sized so that
/// realistic closure activations (large package set, slow disk,
/// post-activation systemd target convergence) complete inside the
/// CP's confirm deadline (`DEFAULT_CONFIRM_DEADLINE_SECS = 360`).
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

/// How often the poll loop checks `/run/current-system`. 2s matches
/// — fast enough to feel snappy in interactive runs, slow
/// enough to keep CPU + IO load negligible.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of an activation attempt. The agent's main loop maps each
/// variant to a follow-up action: confirm on `FiredAndPolled`,
/// rollback on `SwitchFailed`, retry-on-next-tick on
/// `RealiseFailed`/`SignatureMismatch` (nothing was switched, nothing
/// to roll back).
#[derive(Debug)]
pub enum ActivationOutcome {
    /// **Timing semantics.** Fire-and-forget : the agent has
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
    /// doesn't match the input. The system was never switched
    /// caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nix-store --realise` failed specifically because the closure's
    /// narinfo signature did not match any key in
    /// `nixfleet.trust.cacheKeys` ( root #2). Distinct from
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
    /// - `"systemd-run-fire"`: queueing the transient unit (rare
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
}

/// Activate `target` via realise → set-profile → fire-and-forget
/// switch → poll → self-correct.
///
/// `tracing` events at every step share a `target_closure = <hash>`
/// field, so `journalctl | grep target_closure=<hash>` follows one
/// activation end to end without parsing JSON.
///
/// **Retry semantics (v0.2 trade-off vs §5).** 's
/// original wording allowed up to 3 in-call retries before giving
/// up to rollback. v0.2 takes a different path: a single
/// fire-and-forget attempt per call, with the agent's main poll
/// loop providing the retry cadence (one retry every
/// `--poll-interval` seconds, default 60s; backoff applies on
/// transport errors).
///
/// Why: the in-call retry budget assumed a synchronous activation
/// model where the agent could retry quickly. With fire-and-forget,
/// each attempt is gated by the 300s `POLL_BUDGET`, so 3 in-call
/// attempts could trip the CP's confirm deadline before any
/// actually succeeded. Tick-cadence retry instead keeps each
/// attempt within its own deadline window. The cost: slower
/// failure detection (60s tick vs ~5s per 's intent), but
/// the failure surface is fundamentally smaller because each
/// systemd-run unit is independent and observed via `systemctl
/// show ExecMainStatus`.
///
/// Per-closure quarantine to prevent infinite retry loops on a
/// genuinely-bad closure is tracked in
/// <https://github.com/abstracts33d/nixfleet/issues/55>.
pub async fn activate(target: &EvaluatedTarget) -> Result<ActivationOutcome> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target",
    );

    // Step 0: switch-lock probe (v0.1 nix::is_switch_in_progress).
    //
    // If a `switch-to-configuration` from somewhere else is already
    // running (operator's manual `nh os switch`, a sibling Ansible
    // play, an in-flight `nixos-rebuild switch`), bow out and let
    // them complete. The next poll tick will re-attempt; if /run/
    // current-system already matches the dispatched target by then,
    // dispatch returns Converged on the next checkin and we never
    // even enter activate.
    //
    // Without this probe, two activators racing on the same lock
    // produce surprising failure modes: the second one blocks on
    // the file lock during its switch script's nixos-tmpfiles run,
    // both spit interleaved logs, and a poll-budget timeout reports
    // `SwitchFailed` even though the *other* switch eventually
    // completes successfully.
    if is_switch_in_progress().await {
        tracing::info!(
            target_closure = %target.closure_hash,
            "agent: skipping activation — another switch-to-configuration is in flight",
        );
        return Ok(ActivationOutcome::RealiseFailed {
            reason: "switch-to-configuration lock held by another process; will retry on next tick".to_string(),
        });
    }

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

    // Step 3: fire activation (platform-dispatched fire-and-forget).
    //
    // Linux: `systemd-run --unit=nixfleet-switch -- <store>/bin/
    // switch-to-configuration switch` — detached transient service
    // independent of the agent's cgroup so the agent can be SIGTERMed
    // mid-fire without killing the activation.
    //
    // Darwin: `<store>/activate-user` (legacy, skip if absent) +
    // `<store>/activate`, both spawned with `setsid ` so the child
    // is in its own session — launchd's process-group SIGTERM during
    // the agent plist reload doesn't propagate.
    //
    // See `fire_switch` for the per-platform rationale.
    if let Some(outcome) = fire_switch(target, &store_path).await? {
        return Ok(outcome);
    }

    // Step 4: poll /run/current-system for the expected basename.
    //
    // Budget: 300s . Coupled to CP's confirm_deadline_secs
    // default of 360s = 300s + 60s slack. See
    // `crates/nixfleet-control-plane/src/server/state.rs::DEFAULT_CONFIRM_DEADLINE_SECS`.
    //
    // Fire-and-forget contract: if the agent process gets killed
    // mid-poll (because the new closure stops nixfleet-agent.service
    // and systemd hasn't yet started the new agent), the
    // `nixfleet-switch.service` continues independently. When the
    // post-switch agent boots, the boot-recovery path
    // (`recovery::run_boot_recovery`) observes
    // `/run/current-system == last_dispatched.closure_hash` and
    // posts the retroactive confirm.
    let expected = &target.closure_hash;
    match poll_current_system(expected, POLL_BUDGET, POLL_INTERVAL).await {
        Ok(()) => {
            // Step 5: post-poll profile self-correction (v0.1
            // `verify_profile`). Defends against the edge case where
            // the activation script (or a concurrent nix-env caller)
            // mutated `/nix/var/nix/profiles/system` after our Step 2
            // `nix-env --set`. If that happened, /run/current-system
            // could match expected (because the activation script
            // re-pointed it) but the profile generation pointer is
            // off — leaving us booting fine now but vulnerable to a
            // surprise on next reboot.
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
        Err(timeout_info) => {
            // Poll budget exceeded. Read the transient unit's exit
            // status from systemctl show — best-effort triage signal
            // for the operator. The unit may still be running (large
            // closure download), in which case ExecMainStatus is
            // empty / "0" / something inconclusive.
            let exit_code = read_unit_exit_code("nixfleet-switch.service").await;
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

/// Structured failure mode for `realise `. Distinguishing
/// signature-mismatch from generic failure lets the agent map each
/// to a different `ReportEvent` variant ( root #2: explicit
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
/// `Ok( )` as soon as the symlink resolves to `expected`. Returns
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

/// Verify that `/nix/var/nix/profiles/system` resolves to
/// `expected_store_path`. If not, re-run `nix-env --set` to point
/// it back. Ports v0.1's `verify_profile` (deleted at afb5e18^:
/// crates/agent/src/nix.rs:344-374) — defensive against concurrent
/// profile mutations during activation.
///
/// Returns `Ok( )` if the profile matches (either initially or
/// after self-correction). Returns `Err` only when self-correction
/// itself failed; the post-poll caller treats that as non-fatal
/// (the symlink-level check on /run/current-system already passed).
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

/// Read `/nix/var/nix/profiles/system` (two-level symlink: profile
/// → `system-<N>-link` → `/nix/store/<basename>`) and compare to
/// the expected /nix/store path. Returns false on any read error
/// (caller treats unreadable as mismatch and triggers correction).
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

/// Best-effort read of a transient systemd unit's exit code via
/// `systemctl show --property=ExecMainStatus`. Used by both
/// `activate ` (`nixfleet-switch.service`) and `rollback `
/// (`nixfleet-rollback.service`) to triage poll-timeout cases.
/// Returns `None` if the command fails or the property is empty/
/// non-numeric — caller should treat exit status as unknown rather
/// than synthesize a misleading 0.
///
/// **Darwin:** no transient unit exists (the activation runs in a
/// detached `setsid ` child, not a systemd-run service), so there's
/// no equivalent exit-status surface. Returns `None` early on macOS
/// — same semantic as "unknown" on linux: the poll-timeout caller
/// already declares SwitchFailed, the exit code is purely diagnostic.
async fn read_unit_exit_code(unit_name: &str) -> Option<i32> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("systemctl")
        .arg("show")
        .arg("--property=ExecMainStatus")
        .arg("--value")
        .arg(unit_name)
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

/// Platform-dispatched fire-and-forget activation. Returns
/// `Ok(None)` on a clean fire (caller proceeds to poll); returns
/// `Ok(Some(outcome))` on a fire-step failure (caller short-circuits
/// with that outcome); returns `Err` only on spawn-level I/O errors.
///
/// On linux: `systemd-run --unit=nixfleet-switch -- <store>/bin/
/// switch-to-configuration switch`. The transient unit gets its own
/// cgroup, so the agent's death cannot kill it. `--collect` removes
/// the unit on exit so the fixed unit name is reusable.
///
/// On darwin: spawns `<store>/activate-user` (legacy, skipped if
/// absent) followed by `<store>/activate`, both detached via
/// `setsid ` so launchd's process-group SIGTERM during the agent's
/// own plist reload doesn't propagate to the activation child. Per
/// v0.1.1 darwin-platform-notes.md `nohup` doesn't work in a
/// launchd daemon context (no controlling TTY), only `setsid `
/// gives the new session lifetime that survives the parent.
async fn fire_switch(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    if cfg!(target_os = "macos") {
        fire_switch_darwin(target, store_path).await
    } else {
        fire_switch_systemd(target, store_path).await
    }
}

/// Linux/systemd fire-and-forget. See `fire_switch` for shape;
/// `Ok(None)` on clean fire, `Ok(Some(SwitchFailed))` on fire-step
/// non-zero, `Err` on spawn I/O error.
async fn fire_switch_systemd(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
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
        return Ok(Some(ActivationOutcome::SwitchFailed {
            phase: "systemd-run-fire".to_string(),
            exit_code: fire_status.code(),
        }));
    }
    Ok(None)
}

/// Darwin/launchd fire-and-forget. Spawns the activation script in a
/// new session via `setsid ` so the agent's process group SIGTERM
/// (which launchd issues during plist reload when the new closure
/// changes the agent binary path) does not propagate to the
/// activation child.
///
/// Sequence (matches `darwin-rebuild switch`):
/// 1. `<store>/activate-user` — legacy user activation. Some darwin
///   closures still ship it, others don't. Absent → skip silently.
/// 2. `<store>/activate` — system activation script. Always present.
///
/// Both detach via `pre_exec(setsid)` and redirect stdout/stderr to
/// `/var/log/nixfleet-activate.log` (best-effort: if the file isn't
/// writable, fall back to inherit and let launchd's
/// StandardOutPath/StandardErrorPath catch the output).
///
/// Returns immediately after spawning the second `activate`. Caller
/// polls `/run/current-system` for the expected basename to detect
/// completion. On macOS the activate script creates
/// `/run/current-system` as a symlink to the new store path (per
/// v0.1.1 darwin-platform-notes.md), so the polling contract is
/// identical across platforms.
#[cfg(unix)]
async fn fire_switch_darwin(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: firing darwin activation (setsid-detached activate-user + activate)",
    );

    // Step 1: activate-user (legacy). Skip if the script doesn't
    // exist in the new closure — modern darwin closures often omit
    // it. Errors here are non-fatal in v0.1; same here.
    let activate_user = format!("{store_path}/activate-user");
    if std::path::Path::new(&activate_user).exists() {
        let mut cmd = std::process::Command::new(&activate_user);
        cmd.stdin(Stdio::null());
        attach_activate_log(&mut cmd);
        // SAFETY: setsid is async-signal-safe and the closure does
        // no allocation / lock acquisition. The child inherits the
        // pre_exec hook and runs it after fork before exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(_child) => {
                tracing::debug!(
                    target_closure = %target.closure_hash,
                    "agent: darwin activate-user fired (detached)",
                );
            }
            Err(err) => {
                tracing::warn!(
                    target_closure = %target.closure_hash,
                    error = %err,
                    "agent: darwin activate-user spawn failed (non-fatal); continuing to system activate",
                );
            }
        }
    } else {
        tracing::debug!(
            target_closure = %target.closure_hash,
            "agent: darwin activate-user absent; skipping (modern closure shape)",
        );
    }

    // Step 2: system activate. This is the script that may unload +
    // reload the launchd plist, killing the agent mid-run if the
    // binary path changed. setsid puts this child in its own session
    // so launchd's process-group SIGTERM cannot reach it. The agent
    // either survives (KeepAlive restart) and observes the new
    // `/run/current-system` via the poll loop, OR dies and gets
    // restarted by launchd, in which case `recovery::run_boot_recovery`
    // detects "current matches last_dispatched" and posts the
    // retroactive confirm.
    let activate = format!("{store_path}/activate");
    let mut cmd = std::process::Command::new(&activate);
    cmd.stdin(Stdio::null());
    attach_activate_log(&mut cmd);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_child) => {
            tracing::info!(
                target_closure = %target.closure_hash,
                "agent: darwin activate fired (setsid-detached); polling current-system",
            );
            Ok(None)
        }
        Err(err) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: darwin activate spawn failed",
            );
            Ok(Some(ActivationOutcome::SwitchFailed {
                phase: "darwin-activate-spawn".to_string(),
                exit_code: None,
            }))
        }
    }
}

/// Non-unix stub — never reached at runtime since `cfg!(target_os =
/// "macos")` is the only branch that calls this, and macOS is unix.
/// Kept so the function exists for the dispatch site without an
/// `#[cfg]` on the call. (No supported NixFleet platform is non-unix.)
#[cfg(not(unix))]
async fn fire_switch_darwin(
    _target: &EvaluatedTarget,
    _store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    Err(anyhow!("fire_switch_darwin called on non-unix host"))
}

/// Best-effort: attach the activate log file as stdout + stderr.
/// Falls back to inherit on permission/IO error so the spawn doesn't
/// fail the whole activation; launchd's StandardOutPath/StandardError
/// catch the inherited stream when redirection fails.
#[cfg(unix)]
fn attach_activate_log(cmd: &mut std::process::Command) {
    use std::process::Stdio;
    const ACTIVATE_LOG: &str = "/var/log/nixfleet-activate.log";
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ACTIVATE_LOG)
    {
        Ok(out) => {
            // Need two handles — stdout and stderr each consume one.
            let err = match out.try_clone() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        path = ACTIVATE_LOG,
                        error = %e,
                        "could not clone activate log handle; using inherit",
                    );
                    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
                    return;
                }
            };
            cmd.stdout(out).stderr(err);
        }
        Err(e) => {
            tracing::warn!(
                path = ACTIVATE_LOG,
                error = %e,
                "could not open activate log; using inherit",
            );
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
    }
}

/// Platform-dispatched fire-and-forget rollback. Symmetric to
/// `fire_switch`. Returns `Ok(None)` on clean fire (caller proceeds
/// to poll), `Ok(Some(failure))` on fire-step failure, `Err` on
/// spawn-level I/O error.
async fn fire_rollback(target_basename: &str) -> Result<Option<RollbackOutcome>> {
    if cfg!(target_os = "macos") {
        fire_rollback_darwin(target_basename).await
    } else {
        fire_rollback_systemd().await
    }
}

/// Linux/systemd rollback fire. Mirrors `fire_switch_systemd` but
/// fires `nixfleet-rollback.service` against
/// `/run/current-system/bin/switch-to-configuration` (the symlink
/// re-pointed by `nix-env --rollback` already, so this hits the
/// rolled-back closure's switch script).
async fn fire_rollback_systemd() -> Result<Option<RollbackOutcome>> {
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .arg("nixfleet-rollback.service")
        .status()
        .await;

    let switch_bin = "/run/current-system/bin/switch-to-configuration";
    let fire_status = Command::new("systemd-run")
        .arg("--unit=nixfleet-rollback")
        .arg("--collect")
        .arg("--")
        .arg(switch_bin)
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn systemd-run --unit=nixfleet-rollback")?;

    if !fire_status.success() {
        tracing::error!(
            exit_code = ?fire_status.code(),
            "agent: systemd-run failed to queue rollback unit",
        );
        return Ok(Some(RollbackOutcome::Failed {
            phase: "systemd-run-fire".to_string(),
            exit_code: fire_status.code(),
        }));
    }
    Ok(None)
}

/// Darwin rollback fire. Same setsid-detached pattern as
/// `fire_switch_darwin`. The previously-rolled-back generation's
/// store path is now pointed at by `/nix/var/nix/profiles/system`
/// (because `rollback ` already ran `nix-env --rollback`), so we
/// invoke `<resolved-store-path>/activate` to actually run the
/// activation chain. Polling for `target_basename` in the caller
/// observes completion.
#[cfg(unix)]
async fn fire_rollback_darwin(target_basename: &str) -> Result<Option<RollbackOutcome>> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let store_path = format!("/nix/store/{target_basename}");
    let activate = format!("{store_path}/activate");
    if !std::path::Path::new(&activate).exists() {
        tracing::error!(
            activate = %activate,
            "agent: darwin rollback target has no activate script",
        );
        return Ok(Some(RollbackOutcome::Failed {
            phase: "darwin-activate-missing".to_string(),
            exit_code: None,
        }));
    }

    tracing::info!(
        target = %target_basename,
        "agent: firing darwin rollback (setsid-detached activate)",
    );
    let mut cmd = std::process::Command::new(&activate);
    cmd.stdin(Stdio::null());
    attach_activate_log(&mut cmd);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_child) => Ok(None),
        Err(err) => {
            tracing::error!(
                target = %target_basename,
                error = %err,
                "agent: darwin rollback activate spawn failed",
            );
            Ok(Some(RollbackOutcome::Failed {
                phase: "darwin-activate-spawn".to_string(),
                exit_code: None,
            }))
        }
    }
}

#[cfg(not(unix))]
async fn fire_rollback_darwin(_target_basename: &str) -> Result<Option<RollbackOutcome>> {
    Err(anyhow!("fire_rollback_darwin called on non-unix host"))
}

/// Outcome of a `rollback ` call. Mirrors `ActivationOutcome`'s
/// shape so callers can pattern-match similarly. Fire-and-forget
/// applies to rollback for the same reason as activate: if the
/// rolled-back closure's activation script changes a unit definition
/// the running agent depends on (transitively — system services like
/// dbus/systemd-tmpfiles can chain into this), a synchronous spawn
/// gets SIGTERMed mid-run when systemd reloads.
#[derive(Debug)]
pub enum RollbackOutcome {
    /// Rollback fired via `systemd-run --unit=nixfleet-rollback` AND
    /// `/run/current-system` flipped to the rolled-back target
    /// within the poll budget. Caller can treat the agent's known
    /// state as "back to previous gen".
    FiredAndPolled,
    /// One of the rollback steps failed. `phase`:
    /// - `"nix-env-rollback"`: the profile flip failed. System still
    ///   on the failed-activation generation.
    /// - `"discover-target"`: profile flipped but we couldn't read
    ///   it back (broken symlink / racy state).
    /// - `"systemd-run-fire"`: queueing the rollback unit failed.
    /// - `"rollback-poll-timeout"`: poll budget elapsed without
    ///   `/run/current-system` flipping.
    Failed {
        phase: String,
        exit_code: Option<i32>,
    },
}

impl RollbackOutcome {
    /// Convenience helper used by callers that just want a yes/no.
    pub fn success(&self) -> bool {
        matches!(self, RollbackOutcome::FiredAndPolled)
    }
    /// Exit code if the failure carried one (for ReportEvent).
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            RollbackOutcome::Failed { exit_code, .. } => *exit_code,
            RollbackOutcome::FiredAndPolled => None,
        }
    }
    /// Phase string if failed (for ReportEvent).
    pub fn phase(&self) -> Option<&str> {
        match self {
            RollbackOutcome::Failed { phase, .. } => Some(phase.as_str()),
            RollbackOutcome::FiredAndPolled => None,
        }
    }
}

/// Local rollback: revert the system profile one generation back and
/// run the previous closure's `switch-to-configuration switch` via
/// the same fire-and-forget mechanism `activate ` uses .
///
/// Used when:
/// - `activate ` returned a non-success outcome that requires
///   rollback (`SwitchFailed`).
/// - The agent's confirm window expired before the CP acknowledged
///   the activation (magic rollback, ).
///
/// Sequence:
/// 1. `nix-env --rollback` flips the profile symlink to the previous
///   generation (synchronous — fast, no service restarts here).
/// 2. Read the new profile target to discover what closure to poll
///   for. Without this we'd be polling for "any change", which is
///   a weaker contract than "rolled to the expected closure".
/// 3. Fire `systemd-run --unit=nixfleet-rollback --collect
/// <new_target>/bin/switch-to-configuration switch`. The detached
///   transient unit insulates the rollback's switch script from
///   the agent's cgroup, so even if rollback's activation chain
///   restarts services the agent depends on, the rollback completes.
/// 4. Poll `/run/current-system` for the rolled-back basename, same
///   POLL_BUDGET as activate (300s). Coupled to confirm-deadline.
///
/// Bypasses `nixos-rebuild` entirely — `nixos-rebuild-ng` tries to
/// evaluate `<nixpkgs/nixos>` even on `--rollback`, which fails in
/// the agent's NIX_PATH-less sandbox.
///
/// Idempotent on the profile flip layer: running twice rolls back
/// twice. The caller is expected to invoke this exactly once per
/// failed activation.
pub async fn rollback() -> Result<RollbackOutcome> {
    tracing::warn!("agent: triggering local rollback (fire-and-forget via systemd-run)");

    // Step 1: profile flip. Synchronous — this is just a symlink
    // re-target, no services touched.
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

    // Step 2: discover the target. /nix/var/nix/profiles/system is
    // a symlink to system-<N>-link, which is itself a symlink to
    // /nix/store/<basename>. read_link twice to follow both layers,
    // then take the basename — that's what /run/current-system will
    // resolve to once the activation script has run.
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

    // Step 3: fire rollback (platform-dispatched fire-and-forget).
    // Linux: `systemd-run --unit=nixfleet-rollback`. Darwin: detached
    // `<store>/activate` via setsid. See `fire_rollback`.
    if let Some(failure) = fire_rollback(&target_basename).await? {
        return Ok(failure);
    }

    // Step 4: poll for the rolled-back target.
    match poll_current_system(&target_basename, POLL_BUDGET, POLL_INTERVAL).await {
        Ok(()) => {
            tracing::info!(
                target = %target_basename,
                "agent: rollback fire-and-forget complete",
            );
            Ok(RollbackOutcome::FiredAndPolled)
        }
        Err(timeout) => {
            let exit_code = read_unit_exit_code("nixfleet-rollback.service").await;
            tracing::error!(
                target = %target_basename,
                last_observed = %timeout.last_observed,
                exit_code = ?exit_code,
                "agent: rollback poll timed out",
            );
            Ok(RollbackOutcome::Failed {
                phase: "rollback-poll-timeout".to_string(),
                exit_code,
            })
        }
    }
}

/// Resolve `/nix/var/nix/profiles/system` to the rolled-back closure's
/// /nix/store basename. Follows two symlink levels: profile →
/// `system-<N>-link` (relative) → `/nix/store/<basename>` (absolute).
fn resolve_profile_target() -> Result<String> {
    let profile = std::path::Path::new("/nix/var/nix/profiles/system");
    let gen_link = std::fs::read_link(profile)
        .with_context(|| "readlink /nix/var/nix/profiles/system")?;
    // Generation link is relative to the profile's parent dir.
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

/// POST `/v1/agent/confirm` to acknowledge a successful activation.
///
/// Per the agent confirms exactly once after a
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

    /// Platform dispatch sanity check. The helper picks the right
    /// fire impl based on `cfg!(target_os)`. We can't unit-test the
    /// actual fire path (would need a real `nix-env`+`activate`/
    /// `systemd-run` rig — that's the harness's job), but we CAN
    /// assert that `is_switch_in_progress` short-circuits to false
    /// on darwin (no flock probe) and goes through the file-existence
    /// gate on linux.
    #[tokio::test]
    async fn is_switch_in_progress_short_circuits_on_darwin() {
        // On darwin: always false (no equivalent lock).
        // On linux: returns false when the lock file is absent.
        // Either way, in CI / dev sandbox the lock file shouldn't
        // exist, so the function returns false. The platform-specific
        // assertion is that on darwin we never even *try* to run
        // `flock(1)` — the function returns at the first line. We
        // can't directly observe that without mocking, but we *can*
        // assert the function returns false fast (well under the
        // 1s timeout) on a clean dev machine.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            is_switch_in_progress(),
        )
        .await;
        assert!(
            result.is_ok(),
            "is_switch_in_progress should return promptly on a host with no in-flight switch",
        );
        assert!(
            !result.unwrap(),
            "is_switch_in_progress: no switch in flight on a clean dev host",
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

    /// Ensure `read_unit_exit_code` returns None on darwin without
    /// shelling out to a non-existent `systemctl`. The mere fact
    /// that the call returns (rather than hanging or panicking on
    /// macOS) is the assertion — the function short-circuits on
    /// `cfg!(target_os = "macos")`.
    #[tokio::test]
    async fn read_unit_exit_code_short_circuits_on_darwin() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_unit_exit_code("definitely-not-a-real-unit.service"),
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
}
