//! macOS / nix-darwin impl of `HostFacts`.
//!
//! All darwin-specific calls (`sysctl kern.boottime`) live here and
//! nowhere else. The module is gated `cfg(target_os = "macos")`, so
//! nothing inside needs per-symbol gates.
//!
//! `pending_generation` is unconditionally `Ok(None)`:
//! `darwin-rebuild switch` activates in-process with no kernel
//! reboot, so the "booted vs activated" delta surfaced through
//! `/run/booted-system` on Linux does not exist on darwin.

use std::mem::MaybeUninit;

use anyhow::Result;
use nixfleet_proto::agent_wire::PendingGeneration;

use super::HostFacts;

#[derive(Default, Debug, Clone, Copy)]
pub struct DarwinHost;

impl DarwinHost {
    pub fn new() -> Self {
        Self
    }
}

impl HostFacts for DarwinHost {
    /// Read `kern.boottime` via sysctl and format as `<sec>.<usec>`.
    /// The sysctl returns a `struct timeval` carrying the boot
    /// timestamp, stable for the boot session and changing across
    /// reboots — same semantics as Linux's `boot_id`. (macOS has no
    /// equivalent of `/proc`; `IOPlatformUUID` is a HARDWARE id
    /// that does NOT rotate on reboot, so it's the wrong primitive
    /// here.)
    fn boot_id(&self) -> Result<String> {
        let name = std::ffi::CString::new("kern.boottime").expect("static CStr");
        let mut tv: MaybeUninit<libc::timeval> = MaybeUninit::uninit();
        let mut size = std::mem::size_of::<libc::timeval>();
        // SAFETY: sysctlbyname is async-signal-safe; we pass a valid
        // mut pointer to a stack-allocated `timeval` and the matching
        // size. On success the kernel initialises the buffer; we
        // gate the `assume_init` on rc == 0.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                tv.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(
                anyhow::Error::new(std::io::Error::last_os_error()).context("sysctl kern.boottime")
            );
        }
        let tv = unsafe { tv.assume_init() };
        Ok(format!("{}.{:06}", tv.tv_sec, tv.tv_usec))
    }

    fn pending_generation(&self) -> Result<Option<PendingGeneration>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_id_returns_a_non_empty_string() {
        let id = DarwinHost.boot_id().expect("boot_id() must succeed on darwin");
        assert!(!id.is_empty(), "boot_id() returned an empty string");
    }

    #[test]
    fn boot_id_is_stable_within_a_process() {
        let a = DarwinHost.boot_id().unwrap();
        let b = DarwinHost.boot_id().unwrap();
        assert_eq!(a, b, "boot_id must be stable within the running process");
    }

    #[test]
    fn pending_generation_is_always_none() {
        // nix-darwin activates inline; no booted-vs-activated state.
        let p = DarwinHost.pending_generation().unwrap();
        assert!(p.is_none(), "darwin must report no pending generation");
    }
}
