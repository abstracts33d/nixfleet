//! `ActivationBackend` trait + cfg-selected default backend.
//!
//! Method-level docs in `linux.rs` / `darwin.rs` give the per-impl
//! contract. Trait-level guarantees:
//!
//! - `is_switch_in_progress` is fail-open: the caller treats `false`
//!   as "either no contention, OR we couldn't tell" — a false
//!   negative is a stale-lock hazard handled at the lock layer, not
//!   here.
//! - `read_unit_exit_code` returns `None` on any error or absent
//!   surface; the agent never synthesises a misleading 0.
//! - `fire_switch` / `fire_rollback` are "fire-and-forget":
//!   `Ok(None)` means the platform-specific async work was
//!   dispatched and the caller should poll `/run/current-system`.
//!   `Ok(Some(outcome))` means the fire step itself failed; no
//!   poll. `Err` is reserved for spawn-level I/O errors.

use anyhow::Result;
use nixfleet_proto::agent_wire::EvaluatedTarget;

use super::outcome::{ActivationOutcome, RollbackOutcome};

#[cfg(target_os = "linux")]
pub use super::linux::LinuxBackend;
#[cfg(target_os = "macos")]
pub use super::darwin::DarwinBackend;

/// The cfg-selected default backend type — `LinuxBackend` on linux,
/// `DarwinBackend` on macos. Production callers use the const
/// `DEFAULT_BACKEND` rather than constructing one directly.
#[cfg(target_os = "linux")]
pub type DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub type DefaultBackend = DarwinBackend;

/// Process-wide singleton of the cfg-selected backend. Callers
/// outside this module should use the `activate(target)` /
/// `rollback()` façades; tests construct a fake `ActivationBackend`
/// and call the `*_with` form.
#[cfg(target_os = "linux")]
pub const DEFAULT_BACKEND: DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub const DEFAULT_BACKEND: DefaultBackend = DarwinBackend;

/// Platform abstraction. Four primitives — every other piece of the
/// activation pipeline (realise, profile flip, post-verify poll,
/// self-correction) is platform-agnostic and lives in sibling
/// modules.
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
