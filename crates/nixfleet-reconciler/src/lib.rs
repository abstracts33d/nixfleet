//! Pure-function rollout reconciler + RFC-0002 §4 step 0 verification.
//!
//! Two public entry points, intentionally decoupled:
//!
//! - [`verify_artifact`] — step 0: parse + canonicalize + signature-verify
//!   + freshness-check a `fleet.resolved.json` artifact. Returns a verified
//!   [`FleetResolved`] or a [`VerifyError`].
//! - [`reconcile()`] — steps 1–6: pure decision procedure. Takes a verified
//!   [`FleetResolved`], an [`Observed`] state, and `now`; returns
//!   `Vec<`[`Action`]`>`.
//!
//! The CP tick loop calls them in sequence. Tests exercise each
//! independently. Both are stateless: state lives in the inputs.

pub mod action;
pub mod observed;
pub mod reconcile;
pub mod verify;

// Internal modules — logic lives here, extracted from reconcile::reconcile
// after the initial TDD pass (see plan Phase E).
pub(crate) mod budgets;
pub(crate) mod edges;
pub(crate) mod host_state;
pub(crate) mod rollout_state;

pub use action::Action;
pub use nixfleet_proto::FleetResolved;
pub use observed::{HostState, Observed, Rollout};
pub use reconcile::reconcile;
pub use verify::{verify_artifact, verify_revocations, VerifyError};
