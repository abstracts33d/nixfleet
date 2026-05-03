//! Dispatch trait + shared context + signing helper.

use std::future::Future;
use std::sync::Arc;

use nixfleet_proto::agent_wire::EvaluatedTarget;

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;

use crate::Args;

/// Returns `None` for both "not configured" and "configured but failed";
/// the runtime-failure path emits an `error!` so auditors can distinguish them.
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

pub(crate) struct DispatchCtx<'a, R: Reporter> {
    pub target: &'a EvaluatedTarget,
    pub reporter: &'a R,
    pub args: &'a Args,
    pub evidence_signer: &'a Arc<Option<EvidenceSigner>>,
}

/// Telemetry-only: handlers never propagate errors.
pub(crate) trait DispatchHandler {
    fn handle<R: Reporter>(
        &self,
        ctx: &DispatchCtx<'_, R>,
    ) -> impl Future<Output = ()> + Send;
}
