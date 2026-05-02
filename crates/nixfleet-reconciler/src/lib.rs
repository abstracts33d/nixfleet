#![allow(clippy::doc_lazy_continuation)]
//! Pure-function rollout reconciler + step 0 verification.
//!
//! - [`verify_artifact`]: parse, canonicalize, signature-verify and
//!   freshness-check a `fleet.resolved.json` artifact. Returns a
//!   verified [`FleetResolved`] or a [`VerifyError`].
//! - [`reconcile`]: pure decision procedure. Takes a verified
//!   [`FleetResolved`], an [`Observed`] state, and `now`; returns
//!   `Vec<`[`Action`]`>`.
//!
//! Both are stateless: state lives in the inputs.

pub mod action;
pub mod evidence;
pub mod host_state;
pub mod manifest;
pub mod observed;
pub mod reconcile;
pub mod rollout_state;
pub mod verify;

pub use action::Action;
pub use host_state::HostRolloutState;
pub use nixfleet_proto::FleetResolved;
pub use observed::{HostState, Observed, Rollout};
pub use reconcile::reconcile;
pub use rollout_state::RolloutState;
pub use manifest::{compute_rollout_id_for_channel, project_manifest};
pub use verify::{
    compute_canonical_hash, compute_rollout_id, verify_artifact, verify_revocations,
    verify_rollout_manifest, verify_signed_sidecar, SignedSidecar, VerifyError,
};
