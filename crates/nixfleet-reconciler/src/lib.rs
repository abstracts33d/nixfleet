#![allow(clippy::doc_lazy_continuation)]
//! Pure planner + verification primitives for the RFC-0006 runtime.

pub mod evidence;
pub mod manifest;
pub mod planner;
pub mod planner_gates;
pub mod planner_types;
pub mod trust_rotation;
pub mod verify;

pub use manifest::{compute_rollout_id_for_channel, current_rollout_ids, project_manifest};
pub use nixfleet_proto::FleetResolved;
pub use trust_rotation::check_trust_rotations;
pub use verify::{
    SignedSidecar, Verified, VerifiedFleet, VerifiedRolloutManifest, VerifyError,
    canonical_hash_from_bytes, compute_canonical_hash, verify_artifact, verify_bootstrap_nonces,
    verify_revocations, verify_rollout_manifest, verify_signed_sidecar,
};

pub use planner::{active_rollout_for_host, compute_soak_due_at, plan_next};
pub use planner_types::{
    ChannelId, ClosureHash, FleetState, GateBlock, HostId, PlanAction, QuarantineSet, RolloutId,
    RolloutSummary, SignedManifestSet,
};
