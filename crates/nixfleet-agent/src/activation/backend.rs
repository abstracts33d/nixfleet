//! `ActivationBackend` trait + cfg-selected default backend.
//!
//! Trait contract:
//! - `is_switch_in_progress` is fail-open (false = no contender OR unknown).
//! - `read_unit_exit_code` returns `None` rather than synthesising a 0.
//! - `fire_*` are fire-and-forget: `Ok(None)` → caller polls; `Ok(Some)` →
//!   fire-step failure, no poll; `Err` → spawn-level I/O error only.

use anyhow::Result;
use nixfleet_proto::agent_wire::EvaluatedTarget;

use super::outcome::{ActivationOutcome, RollbackOutcome};

#[cfg(target_os = "linux")]
pub use super::linux::LinuxBackend;
#[cfg(target_os = "macos")]
pub use super::darwin::DarwinBackend;

#[cfg(target_os = "linux")]
pub type DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub type DefaultBackend = DarwinBackend;

#[cfg(target_os = "linux")]
pub const DEFAULT_BACKEND: DefaultBackend = LinuxBackend;
#[cfg(target_os = "macos")]
pub const DEFAULT_BACKEND: DefaultBackend = DarwinBackend;

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
