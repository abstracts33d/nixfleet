//! Dispatch path: `process_dispatch_target` + the `DispatchHandler` family.
//! Side-effects route through `&impl Reporter` for unit-testability.

mod activate;
pub(crate) mod compliance;
mod confirm;
mod handler;
mod manifest_error;
mod realise_failed;
mod rollback;
mod verify_mismatch;

pub(crate) use activate::process_dispatch_target;
pub(crate) use rollback::handle_cp_rollback_signal;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use nixfleet_agent::comms::Reporter;
    use nixfleet_agent::evidence_signer::EvidenceSigner;
    use nixfleet_proto::agent_wire::{EvaluatedTarget, ReportEvent};

    use super::handler::{DispatchCtx, DispatchHandler};
    use super::realise_failed::{ClosureSignatureMismatchHandler, RealiseFailedHandler};
    use crate::Args;

    #[derive(Default)]
    struct FakeReporter {
        calls: Mutex<Vec<(Option<String>, ReportEvent)>>,
    }
    impl FakeReporter {
        fn new() -> Self {
            Self::default()
        }
        fn calls(&self) -> Vec<(Option<String>, ReportEvent)> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl Reporter for FakeReporter {
        async fn post_report(&self, rollout: Option<&str>, event: ReportEvent) {
            self.calls
                .lock()
                .unwrap()
                .push((rollout.map(String::from), event));
        }
    }

    fn sample_target() -> EvaluatedTarget {
        EvaluatedTarget {
            closure_hash: "abc123-test".to_string(),
            channel_ref: "stable@feedface".to_string(),
            evaluated_at: chrono::Utc::now(),
            rollout_id: None,
            wave_index: None,
            activate: None,
            signed_at: None,
            freshness_window_secs: None,
            compliance_mode: None,
        }
    }

    fn sample_args() -> Args {
        Args {
            control_plane_url: "https://cp.test".to_string(),
            machine_id: "test-host".to_string(),
            poll_interval: 60,
            trust_file: PathBuf::from("/dev/null"),
            ca_cert: None,
            client_cert: None,
            client_key: None,
            bootstrap_token_file: None,
            state_dir: PathBuf::from("/tmp/nixfleet-test"),
            compliance_gate_mode: None,
            ssh_host_key_file: PathBuf::from("/dev/null"),
        }
    }

    fn ctx<'a, R: Reporter>(
        target: &'a EvaluatedTarget,
        reporter: &'a R,
        args: &'a Args,
        signer: &'a Arc<Option<EvidenceSigner>>,
    ) -> DispatchCtx<'a, R> {
        DispatchCtx {
            target,
            reporter,
            args,
            evidence_signer: signer,
        }
    }

    #[tokio::test]
    async fn closure_signature_mismatch_handler_posts_signed_event_and_does_not_attempt_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer: Arc<Option<EvidenceSigner>> = Arc::new(None);

        ClosureSignatureMismatchHandler {
            closure_hash: "abc123-bad-sig".to_string(),
            stderr_tail: "error: lacks a valid signature".to_string(),
        }
        .handle(&ctx(&target, &fake, &args, &signer))
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "expected exactly one post; got {:?}", calls);
        let (rollout, event) = &calls[0];
        assert_eq!(rollout.as_deref(), Some("stable@feedface"));
        match event {
            ReportEvent::ClosureSignatureMismatch {
                closure_hash,
                stderr_tail,
                signature,
            } => {
                assert_eq!(closure_hash, "abc123-bad-sig");
                assert_eq!(stderr_tail, "error: lacks a valid signature");
                assert!(
                    signature.is_none(),
                    "no evidence_signer wired → signature must be None",
                );
            }
            other => panic!("expected ClosureSignatureMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn realise_failed_handler_posts_one_event_no_rollback() {
        let fake = FakeReporter::new();
        let target = sample_target();
        let args = sample_args();
        let signer: Arc<Option<EvidenceSigner>> = Arc::new(None);

        RealiseFailedHandler {
            reason: "network unreachable".to_string(),
        }
        .handle(&ctx(&target, &fake, &args, &signer))
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0].1 {
            ReportEvent::RealiseFailed {
                closure_hash,
                reason,
                ..
            } => {
                assert_eq!(closure_hash, "abc123-test");
                assert_eq!(reason, "network unreachable");
            }
            other => panic!("expected RealiseFailed, got {other:?}"),
        }
    }
}
