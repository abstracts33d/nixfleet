//! Dispatch trait + shared context + signing helper.
//!
//! Each `DispatchHandler` impl in a sibling module consumes a
//! CP-issued failure variant (one per `activation::ActivationOutcome`
//! failure case, plus the manifest-gate failures and the spawn-error
//! catch-all) and emits a signed `ReportEvent` via the [`Reporter`]
//! trait — optionally chaining into a local rollback + follow-up
//! event.
//!
//! Side-effects route through `&impl Reporter`, so handlers are
//! unit-testable with a capturing fake — see the test module in
//! `dispatch/mod.rs`.

use std::future::Future;
use std::sync::Arc;

use nixfleet_proto::agent_wire::EvaluatedTarget;

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;

use crate::Args;

/// Sign `payload` and surface signing failures at `error!` so the
/// silent-fail mode is observable in operator dashboards.
///
/// Returns `None` if the signer wasn't configured (operator opted out
/// of signed evidence) OR if signing failed at runtime. The two cases
/// are distinguished by the log: a configured-but-failed sign emits an
/// `error!` line with the cause; a not-configured signer emits
/// nothing. Without this helper the call sites used `.sign(...).ok()`
/// which collapsed the two cases into "just no signature" — auditors
/// reading the resulting unsigned event couldn't tell whether the
/// agent was meant to be signing and silently broke.
pub(super) fn try_sign<T: serde::Serialize>(
    signer: &EvidenceSigner,
    payload: &T,
) -> Option<String> {
    match signer.sign(payload) {
        Ok(sig) => Some(sig),
        Err(err) => {
            tracing::error!(
                error = ?err,
                "evidence_signer.sign failed; posting unsigned event \
                 (signing was configured, runtime failure)",
            );
            None
        }
    }
}

/// Shared context for every `DispatchHandler` impl: the target being
/// acted on, the wire reporter, the agent CLI args (hostname,
/// state-dir, …) and the optional evidence signer.
pub(crate) struct DispatchCtx<'a, R: Reporter> {
    pub target: &'a EvaluatedTarget,
    pub reporter: &'a R,
    pub args: &'a Args,
    pub evidence_signer: &'a Arc<Option<EvidenceSigner>>,
}

/// Handler for a single dispatch-time failure. Each impl builds its
/// specific signed payload, posts via `ctx.reporter`, and decides
/// whether to call rollback + emit a follow-up event.
///
/// Telemetry-only: handlers never propagate errors. Anything that
/// could fail (rollback shell-out, reporter post) is logged and
/// dropped — the activation loop continues.
pub(crate) trait DispatchHandler {
    fn handle<R: Reporter>(
        &self,
        ctx: &DispatchCtx<'_, R>,
    ) -> impl Future<Output = ()> + Send;
}
