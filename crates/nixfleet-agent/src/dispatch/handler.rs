//! Dispatch trait + shared context. `try_sign` lives in `evidence_signer` lib.

use std::future::Future;
use std::sync::Arc;

use nixfleet_proto::agent_wire::EvaluatedTarget;

use nixfleet_agent::comms::Reporter;
use nixfleet_agent::evidence_signer::EvidenceSigner;

use crate::Args;

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
