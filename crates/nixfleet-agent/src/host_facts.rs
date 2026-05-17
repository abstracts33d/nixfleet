//! Per-host OS primitives (`boot_id`, `pending_generation`); cfg-gated re-export.

use std::path::Path;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::GenerationRef;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub use darwin::{boot_id, pending_generation};
#[cfg(target_os = "linux")]
pub use linux::{boot_id, pending_generation};

/// `/run/current-system` symlink target's basename = the activated
/// system's closure hash. The agent doesn't trust CP's "what closure
/// did you activate" reads; it always reports what the OS says.
///
/// Moved out of the deleted `checkin_state` module so `host_facts`
/// stays self-contained.
pub const CURRENT_SYSTEM: &str = "/run/current-system";

pub fn current_closure_hash() -> Result<String> {
    let target =
        std::fs::read_link(CURRENT_SYSTEM).with_context(|| format!("readlink {CURRENT_SYSTEM}"))?;
    Ok(closure_hash_from_path(&target))
}

/// FOOTGUN: returns full basename, NOT 32-char prefix — byte-equality
/// required across CP / CI / agent.
pub(crate) fn closure_hash_from_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.rsplit('/')
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| s.to_string())
}

/// `channel_ref` is `None` until the projection correlates it.
pub fn current_generation_ref() -> Result<GenerationRef> {
    Ok(GenerationRef {
        closure_hash: current_closure_hash()?,
        channel_ref: None,
        boot_id: boot_id()?,
    })
}
