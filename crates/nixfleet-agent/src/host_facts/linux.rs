//! Linux/NixOS impl of `HostFacts`.
//!
//! All Linux-specific paths (`/proc/sys/kernel/random/boot_id`,
//! `/run/booted-system`) live here and nowhere else. The module
//! itself is gated `cfg(target_os = "linux")`, so nothing inside
//! needs per-symbol gates.

use std::fs;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::PendingGeneration;

use super::HostFacts;
use crate::checkin_state::{closure_hash_from_path, current_closure_hash};

/// Path to the symlink pointing at the system that booted. When
/// this differs from `/run/current-system`, the host has a pending
/// generation queued for next reboot.
const BOOTED_SYSTEM: &str = "/run/booted-system";

/// Linux's per-boot UUID. Stable for a single boot; rotates on
/// reboot.
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Default, Debug, Clone, Copy)]
pub struct LinuxHost;

impl LinuxHost {
    pub fn new() -> Self {
        Self
    }
}

impl HostFacts for LinuxHost {
    fn boot_id(&self) -> Result<String> {
        let raw = fs::read_to_string(BOOT_ID_PATH)
            .with_context(|| format!("read {BOOT_ID_PATH}"))?;
        Ok(raw.trim().to_string())
    }

    fn pending_generation(&self) -> Result<Option<PendingGeneration>> {
        let current = current_closure_hash()?;
        let booted = booted_closure_hash()?;
        if current == booted {
            return Ok(None);
        }
        Ok(Some(PendingGeneration {
            closure_hash: current,
            scheduled_for: None,
        }))
    }
}

fn booted_closure_hash() -> Result<String> {
    let target = fs::read_link(BOOTED_SYSTEM)
        .with_context(|| format!("readlink {BOOTED_SYSTEM}"))?;
    Ok(closure_hash_from_path(&target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_id_returns_a_non_empty_string() {
        let id = LinuxHost.boot_id().expect("boot_id() must succeed on linux");
        assert!(!id.is_empty(), "boot_id() returned an empty string");
    }

    #[test]
    fn boot_id_is_stable_within_a_process() {
        let a = LinuxHost.boot_id().unwrap();
        let b = LinuxHost.boot_id().unwrap();
        assert_eq!(a, b, "boot_id must be stable within the running process");
    }
}
