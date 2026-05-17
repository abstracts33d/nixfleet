//! Agent runtime: MPSC reducer loop + applier + workers (RFC-0009 §7.1).
//!
//! Symmetric to the CP-side runtime in
//! [`nixfleet_control_plane::runtime`]. Workers (probe, activation,
//! longpoll, heartbeat, advance_ticker) feed a single MPSC channel; the
//! reducer task is the sole `nixfleet_state_machine::step` caller; the
//! applier executes `Local*` effects (FireSwitch, FireRollbackTo,
//! ResetProbeCache, EmitEvent).
//!
//! ```text
//!   probe ─────────┐
//!   activation ────┤
//!   longpoll ──────┼──▶  mpsc::Sender<ReducerInput>  ───▶  reducer task ───▶ applier
//!   heartbeat ─────┤                                          │
//!   advance_ticker ┘                                          ▼
//!                                                           (step)
//! ```
//!
//! Invariants (mirror CP's `runtime::mod` — RFC-0009 §2 principle 4):
//!
//! 1. **One MPSC, one mutator.** The reducer task is the only thing that
//!    calls [`nixfleet_state_machine::step`]. Workers emit
//!    [`ReducerInput`] values; the applier executes the produced effects.
//!    A second writer "for performance" is the defect class the v0.2 fold
//!    folded away — don't reintroduce it.
//!
//! 2. **Agent runs the SAME `step()` as the CP mirror.** No "leaner"
//!    agent-side reimplementation. Identical transitions across the
//!    `Local*` / `Remote*` event pairs by construction.
//!
//! 3. **Shutdown via oneshot drop.** The reducer task owns a
//!    `Vec<oneshot::Sender<()>>`; each worker holds the matching
//!    [`ShutdownToken`] wrapping a `Receiver`. Reducer exit drops every
//!    sender; every worker's `select!` shutdown arm fires; workers exit
//!    cleanly.
//!
//! 4. **Bounded channels with sizing rationale.** Capacities are
//!    documented inline. No `unbounded`.
//!
//! 5. **No `chrono::Utc::now()` in the runtime.** Use the `ClockHandle`
//!    abstraction so tests can advance time deterministically and the
//!    runtime can't drift from any caller's notion of "now".

use std::sync::Arc;

use nixfleet_proto::clock::ClockHandle;
use nixfleet_state_machine::Event;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub mod applier;
pub mod outbound_queue;
pub mod recovery;
mod reducer;
pub mod wire;
pub mod workers;

pub use outbound_queue::{OutboundQueue, QueuedEvent};
pub use recovery::{RecoveryOutcome, handshake as boot_recovery_handshake};

/// Reducer input channel depth.
///
/// Sizing rationale: a single agent's peak burst during one rollout
/// cycle is roughly:
///   - 1 `LocalActivate` (from longpoll)
///   - 1 `LocalActivationStarted` + 1 `LocalActivationCompleted`
///   - per declared probe (~1–10): `LocalProbeObservedFirst` then
///     `LocalProbeResult` every interval until soak elapses
///   - 1 `LocalConvergedReached`
/// 32 covers the spike comfortably; 64 doubles for slack. The CP side
/// uses 1024 because one CP fans 256 hosts; one agent only fans
/// itself.
const REDUCER_INPUT_CAPACITY: usize = 64;

/// Reducer-task inputs.
///
/// - [`ReducerInput::HostEvent`] — workers synthesised a `Local*` event;
///   reducer calls `step()` on the agent's `HostRolloutState` for the
///   given `rollout_id`, then runs the applier on the produced effects.
/// - [`ReducerInput::AgentAdvanceTick`] — periodic timer (mirror of
///   CP's `PlanTick`). Drives sustained-failure detection: when a
///   Soaking host's `probe_failure_first_at` exceeds the rollout's
///   policy threshold the reducer synthesises
///   `LocalSustainedFailureCrossed` and feeds it through `step()`.
pub enum ReducerInput {
    HostEvent {
        rollout_id: nixfleet_proto::RolloutId,
        event: Event,
    },
    AgentAdvanceTick,
    /// Refresh the reducer's cached `SignedManifestSet`. Emitted by
    /// the longpoll worker (after `verify_rollout_manifest` succeeds)
    /// + by the boot-recovery handshake (7f). The reducer needs a
    /// cached manifest to look up `RolloutPolicy` for `step()` and to
    /// resolve `channel`/`target_closure` when bootstrapping
    /// `LocalActivate`.
    ManifestSetUpdated(Box<nixfleet_reconciler::planner_types::SignedManifestSet>),
}

/// Shutdown signal handed to a worker at spawn time. Reducer-task exit
/// drops the matching `Sender<()>` and the receiver resolves with
/// `Err(RecvError)`; the worker's `select!` shutdown arm fires and the
/// task exits cleanly.
pub struct ShutdownToken(oneshot::Receiver<()>);

impl ShutdownToken {
    /// Unwrap to the inner `Receiver<()>` for direct use in `select!`.
    pub fn into_inner(self) -> oneshot::Receiver<()> {
        self.0
    }

    /// Test-only constructor. Integration tests spawn a single worker
    /// in isolation (e.g., `integration_mtls.rs::longpoll_worker_*`) and
    /// own their shutdown channel directly; production code goes
    /// through [`runtime::spawn`] which constructs the token internally.
    ///
    /// Available only when the crate is compiled with `#[cfg(test)]`
    /// (the crate's own lib tests) or with the `test-helpers` feature
    /// flag (integration tests via the test target's
    /// `required-features`). Matches the
    /// `nixfleet_reconciler::Verified::unverified_for_tests` escape-
    /// hatch convention so production builds cannot construct a fake
    /// `ShutdownToken`.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn __test_only_from_rx(rx: oneshot::Receiver<()>) -> Self {
        Self(rx)
    }
}

/// Handle returned by [`spawn`].
pub struct RuntimeHandle {
    pub input_tx: mpsc::Sender<ReducerInput>,
    pub reducer_handle: JoinHandle<()>,
    pub worker_handles: Vec<JoinHandle<()>>,
}

impl RuntimeHandle {
    pub fn into_join_handles(self) -> Vec<JoinHandle<()>> {
        let mut handles = self.worker_handles;
        handles.push(self.reducer_handle);
        handles
    }
}

/// Static runtime configuration. Cheap to clone; the workers each get a
/// reference. Built once at startup from CLI args.
///
/// `ca_cert` / `client_cert` / `client_key` carry the same paths the CLI
/// receives (`Args::ca_cert` etc.) and the first-boot enrollment path
/// already uses (`main.rs::maybe_run_first_boot_enrollment`). Workers
/// pass them straight through to `crate::comms::build_client` so every
/// post-enroll `/v1/agent/*` request rides the agent's enrollment-cert
/// mTLS identity. DEFECT-003 regression-guard: any worker building a
/// bare `reqwest::Client` (no client cert) would talk to CP
/// unauthenticated; `integration_mtls.rs` pins the property end-to-end.
#[derive(Clone)]
pub struct AgentConfig {
    pub control_plane_url: String,
    pub machine_id: String,
    pub state_dir: std::path::PathBuf,
    /// LOADBEARING: same `--trust-file` path the binary validated at
    /// startup. Workers that fetch + verify signed manifests
    /// (`manifest_poll`, `longpoll`) MUST read this — the startup check
    /// alone doesn't propagate into runtime trust loading.
    pub trust_file: std::path::PathBuf,
    pub ca_cert: Option<std::path::PathBuf>,
    pub client_cert: Option<std::path::PathBuf>,
    pub client_key: Option<std::path::PathBuf>,
}

/// Spawn the reducer + worker constellation.
///
/// `clock` and `cfg` are the only external dependencies. Workers receive
/// what they need by parameter (no `state` god-object yet — the agent's
/// reducer holds its own state and exposes operations via the input
/// MPSC).
/// Sender end of the applier → activation worker intent channel.
pub type ActivationIntentTx = mpsc::Sender<wire::ActivationIntent>;
/// Sender end of the applier → probe worker reset channel.
pub type ProbeResetTx = mpsc::Sender<wire::ProbeResetCommand>;
/// Watch channel the applier hits after enqueueing a fresh outbound
/// event; wakes the outbound drainer worker immediately rather than
/// waiting for its next periodic tick.
pub type OutboundKickTx = tokio::sync::watch::Sender<()>;

/// Context handed to `applier::apply_effect`. Bundles the channels +
/// reducer input sender the applier needs to dispatch a `Local*` effect
/// without inlining platform-specific code or HTTP. Mirrors the CP-side
/// `ApplierCtx`.
pub struct ApplierCtx<'a> {
    pub cfg: &'a AgentConfig,
    pub clock: &'a ClockHandle,
    pub input_tx: &'a mpsc::Sender<ReducerInput>,
    pub activation_tx: &'a ActivationIntentTx,
    pub probe_reset_tx: &'a ProbeResetTx,
    pub outbound_queue: &'a Arc<OutboundQueue>,
    pub outbound_kick: &'a OutboundKickTx,
}

pub fn spawn(cancel: CancellationToken, cfg: AgentConfig, clock: ClockHandle) -> RuntimeHandle {
    let (input_tx, input_rx) = mpsc::channel::<ReducerInput>(REDUCER_INPUT_CAPACITY);
    // Applier → activation worker. Small bounded queue: the applier
    // emits at most one ActivationIntent per LocalFireSwitch /
    // LocalFireRollbackTo, and the agent can have at most one in-flight
    // switch at a time. Cap 4 covers retry bursts without unbounded
    // growth.
    let (activation_tx, activation_rx) = mpsc::channel::<wire::ActivationIntent>(4);
    // Applier → probe worker reset channel. Same shape; reset is a
    // single signal per `LocalActivationCompleted`.
    let (probe_reset_tx, probe_reset_rx) = mpsc::channel::<wire::ProbeResetCommand>(4);
    // Applier → outbound drainer kick. Watch semantics: a burst of
    // enqueues collapses to one wake, the drainer's own ticker stays
    // as a safety net if the watch is ever missed.
    let (outbound_kick_tx, outbound_kick_rx) = tokio::sync::watch::channel(());

    // Open / create the durable outbound queue on disk. Per Plan 07,
    // every outbound event must hit disk before the network POST so
    // an agent crash between local state change + CP receipt is
    // recoverable on restart. Failure to open the queue is fatal at
    // startup; we'd otherwise lose events silently.
    let outbound_queue = Arc::new(OutboundQueue::open(&cfg.state_dir).unwrap_or_else(|err| {
        panic!(
            "open durable outbound queue under {}: {err:#}",
            cfg.state_dir.display(),
        )
    }));

    // One oneshot per worker. Reducer task takes ownership of the
    // senders; on reducer exit every sender drops, every worker's
    // `select!` shutdown arm fires. Seven workers now: probe,
    // activation, longpoll, heartbeat, advance_ticker, outbound,
    // manifest_poll.
    const SHUTDOWN_TOKEN_COUNT: usize = 7;
    let mut shutdown_senders: Vec<oneshot::Sender<()>> = Vec::new();
    let mut shutdown_tokens: Vec<ShutdownToken> = Vec::new();
    for _ in 0..SHUTDOWN_TOKEN_COUNT {
        let (tx, rx) = oneshot::channel::<()>();
        shutdown_senders.push(tx);
        shutdown_tokens.push(ShutdownToken(rx));
    }
    // Pop in reverse so each consumer gets a distinct token.
    let manifest_poll_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let outbound_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let advance_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let heartbeat_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let longpoll_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let activation_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");
    let probe_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT allocated");

    let worker_handles = vec![
        workers::probe::spawn(
            cfg.clone(),
            clock.clone(),
            input_tx.clone(),
            probe_reset_rx,
            probe_shutdown,
        ),
        workers::activation::spawn(
            cfg.clone(),
            clock.clone(),
            input_tx.clone(),
            activation_rx,
            activation_shutdown,
        ),
        workers::longpoll::spawn(
            cfg.clone(),
            clock.clone(),
            input_tx.clone(),
            longpoll_shutdown,
        ),
        workers::heartbeat::spawn(
            cfg.clone(),
            clock.clone(),
            input_tx.clone(),
            heartbeat_shutdown,
        ),
        workers::advance_ticker::spawn(clock.clone(), input_tx.clone(), advance_shutdown),
        workers::outbound::spawn(
            cfg.clone(),
            outbound_queue.clone(),
            outbound_kick_rx,
            input_tx.clone(),
            outbound_shutdown,
        ),
        workers::manifest_poll::spawn(
            cfg.clone(),
            clock.clone(),
            input_tx.clone(),
            manifest_poll_shutdown,
        ),
    ];

    let reducer_handle = tokio::spawn(reducer::run(
        cancel,
        cfg,
        clock,
        input_rx,
        input_tx.clone(),
        activation_tx,
        probe_reset_tx,
        outbound_queue,
        outbound_kick_tx,
        shutdown_senders,
    ));

    RuntimeHandle {
        input_tx,
        reducer_handle,
        worker_handles,
    }
}

/// Used internally by `spawn` to release worker shutdown receivers when
/// the reducer task exits. Held in scope by the reducer body so dropping
/// it signals all workers in one go.
pub(crate) struct ShutdownGuard(#[allow(dead_code)] pub Vec<oneshot::Sender<()>>);

/// Convenience: cloneable Arc<AgentConfig> for tests / external callers.
pub type AgentConfigHandle = Arc<AgentConfig>;
