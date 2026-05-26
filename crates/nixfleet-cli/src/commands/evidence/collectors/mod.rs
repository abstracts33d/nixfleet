//! `EvidenceCollector` adapter registry.
//!
//! M1.2 ships `nix-derivation` (intent side). M1.3 ships `facter`
//! (hardware side, subprocess-only for GPL v3 hygiene). `osquery`
//! lands here next: runtime-state snapshot via the on-host
//! `compliance.evidence.osquery` producer (subprocess invocation of
//! `osqueryi -A schedule` — Apache 2.0 / GPL v2 dual, no vendoring).
//! Post-M1 adapters: `tpm2-quote`, `securix-attest`.

pub mod facter;
pub mod nix_derivation;
pub mod osquery;
