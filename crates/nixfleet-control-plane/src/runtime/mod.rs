//! CP runtime: MPSC reducer loop + applier + workers (RFC-0009 §7.2).
//!
//! Architecture
//! ============
//!
//! ```text
//!   manifest_poll ─┐
//!   event_ingest  ─┼──▶  mpsc::Sender<ReducerInput>  ───▶  reducer task ───▶ applier
//!   heartbeat_rx  ─┤                                          │
//!   dispatch lp   ─┘                                          ▼
//!                                                       (step + plan_next)
//! ```
//!
//! Invariants (do not violate without an RFC amendment):
//!
//! 1. **One MPSC, one mutator.** The reducer task is the only thing that
//!    calls [`nixfleet_state_machine::step`] or
//!    [`nixfleet_reconciler::planner::plan_next`]. Workers emit
//!    [`ReducerInput`] values; the applier executes the resulting effects.
//!    A second writer "for performance" is the defect class that the v0.2
//!    fold is folding away — don't reintroduce it.
//!
//! 2. **CP mirror runs the same `step()` as the agent.** Identical
//!    transitions for `Local*` and `Remote*` event pairs by construction
//!    (RFC-0009 §2 principle 4). No "leaner" CP-side reimplementation.
//!
//! 3. **Shutdown via oneshot drop.** The reducer task owns a vector of
//!    [`tokio::sync::oneshot::Sender<()>`]; each worker holds the matching
//!    [`ShutdownToken`] (wrapping a `Receiver`). Reducer exit drops every
//!    sender, every worker's `select!` shutdown arm fires, workers exit
//!    cleanly. No leaks if a worker hangs on its own — the `JoinHandle`
//!    drain in `server::mod` aborts past the deadline.
//!
//! 4. **Bounded channels.** The input MPSC and any internal applier queues
//!    have explicit `bounded` capacities with a sizing rationale in a
//!    comment — never `unbounded`, never "let's start at 4096 and see."

use std::sync::Arc;

use chrono::{DateTime, Utc};
use nixfleet_proto::clock::ClockHandle;
use nixfleet_state_machine::Event;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::server::AppState;

pub mod applier;
pub mod event_log_writer;
mod reducer;
pub mod workers;

pub use event_log_writer::{EVENT_LOG_CHANNEL_CAPACITY, EventLogTx};

/// Input channel depth. Sized for ~512 in-flight events plus headroom for
/// burst: a 256-host fleet, ~2 events/host during peak rollout activation,
/// rounded to power-of-two and doubled. Backpressure surfaces at HTTP
/// handlers via `try_send` → 503 (caller retries), preserving the pull-only
/// agent contract (RFC-0008 §2.1 — CP never pushes, agents always retry).
const REDUCER_INPUT_CAPACITY: usize = 1024;

/// Reducer-task inputs. Workers and HTTP route handlers obtain a
/// `mpsc::Sender<ReducerInput>` from the [`RuntimeHandle`] and emit values
/// of this type. Variants partition by the reducer action they drive:
///
/// - [`ReducerInput::HostEvent`] — call `step()` on the CP-mirror per-host
///   state for `(host, rollout_id)`, run applier on resulting effects.
/// - [`ReducerInput::ManifestSetUpdated`] — refresh the cached
///   `SignedManifestSet` and run `plan_next()`.
/// - [`ReducerInput::HeartbeatReceived`] — touch `last_heartbeat_at`; if
///   the agent's `current_closure` disagrees with the CP mirror, the reply
///   contains the last-known-seq for `Replay-From` (RFC-0008 §4.3).
/// - [`ReducerInput::PlanTick`] — safety-net periodic re-run of
///   `plan_next()` in case a manifest update was missed by a poller crash.
pub enum ReducerInput {
    HostEvent {
        host: String,
        rollout_id: nixfleet_proto::RolloutId,
        event: Event,
    },
    ManifestSetUpdated(Box<nixfleet_reconciler::planner_types::SignedManifestSet>),
    HeartbeatReceived {
        host: String,
        rollout_id: Option<nixfleet_proto::RolloutId>,
        current_closure: Option<String>,
        at: DateTime<Utc>,
        reply: oneshot::Sender<HeartbeatReply>,
    },
    PlanTick,
}

/// Heartbeat reply: drift detected ⇒ `replay_from` is `Some(last_known_seq)`;
/// agent should re-POST events from that seq onward (RFC-0008 §4.3).
/// `bootstrap_rollouts` (LIFT #3) carries CP's view of active rollouts the
/// agent should rehydrate when its reducer was lost (boot-recovery shape;
/// heartbeat carried `rollout_id = None` but CP holds non-terminal records
/// for the host).
#[derive(Debug, Clone)]
pub struct HeartbeatReply {
    pub replay_from: Option<u64>,
    pub bootstrap_rollouts: Vec<nixfleet_proto::agent_wire::HostRolloutSnapshot>,
}

/// Shutdown signal handed to a worker at spawn time. Workers select! between
/// their input source and `&mut self.0`; reducer-task exit drops the matched
/// `Sender<()>` and the receiver resolves with `Err(RecvError)`. Workers
/// treat any resolution as "shutdown" and exit cleanly.
pub struct ShutdownToken(oneshot::Receiver<()>);

impl ShutdownToken {
    /// Unwrap to the inner `Receiver<()>` for direct use in `select!`. The
    /// idiomatic pattern is:
    ///
    /// ```ignore
    /// let mut shutdown = token.into_inner();
    /// loop {
    ///     tokio::select! {
    ///         _ = &mut shutdown => return,
    ///         msg = input.recv() => { ... }
    ///     }
    /// }
    /// ```
    pub fn into_inner(self) -> oneshot::Receiver<()> {
        self.0
    }
}

/// Handle returned by [`spawn`]. Holds the input channel sender (cloneable
/// for HTTP route handlers) and the reducer-task `JoinHandle`. The handle
/// must outlive every cloned sender, otherwise the reducer task exits
/// prematurely.
pub struct RuntimeHandle {
    pub input_tx: mpsc::Sender<ReducerInput>,
    /// Cloneable handle to the event_log writer task. Route handlers and
    /// the reducer-task applier send `EventLogEntry` values here; the
    /// dedicated writer task drains and persists. See `event_log_writer`
    /// for the backpressure rationale.
    pub event_log_tx: EventLogTx,
    pub reducer_handle: JoinHandle<()>,
    pub event_log_writer_handle: JoinHandle<()>,
    pub worker_handles: Vec<JoinHandle<()>>,
}

impl RuntimeHandle {
    /// All background tasks owned by this runtime. The CP `server::serve`
    /// drains these as part of the shutdown protocol (`drain_background_tasks`).
    pub fn into_join_handles(self) -> Vec<JoinHandle<()>> {
        let mut handles = self.worker_handles;
        handles.push(self.reducer_handle);
        handles.push(self.event_log_writer_handle);
        handles
    }
}

/// Spawn the reducer + worker constellation. Returns a [`RuntimeHandle`]
/// the caller wires into the server shutdown drain.
///
/// `clock` and `state` are the only external dependencies. Workers
/// obtain everything else (DB handles, manifest cache, etc.) from `state`.
pub fn spawn(cancel: CancellationToken, state: Arc<AppState>, clock: ClockHandle) -> RuntimeHandle {
    let (input_tx, input_rx) = mpsc::channel::<ReducerInput>(REDUCER_INPUT_CAPACITY);
    let (event_log_tx, event_log_rx) =
        mpsc::channel::<crate::db::event_log::EventLogEntry>(EVENT_LOG_CHANNEL_CAPACITY);

    // One oneshot per child task. Reducer task takes ownership of the
    // senders in a `ShutdownGuard`; on reducer exit the guard drops, every
    // sender drops, every child task's `select!` shutdown arm fires.
    // Two tokens: 1 manifest-poll worker + 1 event-log writer. The HTTP
    // route handlers (events / heartbeat / dispatch long-poll) write
    // `ReducerInput` directly to `input_tx`; they coordinate shutdown
    // through axum-server's graceful-drain protocol, not through this
    // constellation (Phase 8f deleted the stub workers).
    const SHUTDOWN_TOKEN_COUNT: usize = 2;
    let mut shutdown_senders: Vec<oneshot::Sender<()>> = Vec::new();
    let mut shutdown_tokens: Vec<ShutdownToken> = Vec::new();
    for _ in 0..SHUTDOWN_TOKEN_COUNT {
        let (tx, rx) = oneshot::channel::<()>();
        shutdown_senders.push(tx);
        shutdown_tokens.push(ShutdownToken(rx));
    }
    // Pop in reverse so each consumer gets a distinct token.
    let event_log_writer_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT tokens allocated");
    let manifest_poll_shutdown = shutdown_tokens
        .pop()
        .expect("SHUTDOWN_TOKEN_COUNT tokens allocated");

    let worker_handles = vec![workers::manifest_poll::spawn(
        state.clone(),
        clock.clone(),
        input_tx.clone(),
        manifest_poll_shutdown,
    )];

    let event_log_writer_handle = match state.db.clone() {
        Some(db) => event_log_writer::spawn(db, event_log_rx, event_log_writer_shutdown),
        None => spawn_drain_only_writer(event_log_rx, event_log_writer_shutdown),
    };

    let reducer_handle = tokio::spawn({
        let event_log_tx = event_log_tx.clone();
        async move {
            reducer::run(
                cancel,
                state,
                clock,
                input_rx,
                event_log_tx,
                shutdown_senders,
            )
            .await
        }
    });

    RuntimeHandle {
        input_tx,
        event_log_tx,
        reducer_handle,
        event_log_writer_handle,
        worker_handles,
    }
}

/// Fallback writer when CP runs in in-memory mode (no `db_path` flag). Just
/// drains entries silently so the channel never fills up. Production paths
/// always configure a DB.
fn spawn_drain_only_writer(
    mut rx: mpsc::Receiver<crate::db::event_log::EventLogEntry>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => return,
                maybe = rx.recv() => {
                    if maybe.is_none() { return; }
                }
            }
        }
    })
}
