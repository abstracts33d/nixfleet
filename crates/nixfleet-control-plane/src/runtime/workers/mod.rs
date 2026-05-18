//! Workers — the I/O-bearing edges around the pure reducer.
//!
//! Each worker is spawned by [`super::spawn`] with its own
//! [`super::ShutdownToken`] and a clone of the reducer's input
//! `mpsc::Sender<ReducerInput>`. Workers never call `step()` or
//! `plan_next()` — they translate I/O into [`super::ReducerInput`] values
//! and let the reducer task do the actual transitions.
//!
//! Routing (RFC-0006 §7.2):
//!
//! - [`manifest_poll`] — periodic verify of channel-refs + per-channel
//!   rollout manifests; emits `ManifestSetUpdated`.
//!
//! The other three route surfaces (`POST /v1/agent/events`,
//! `POST /v1/agent/heartbeat`, `GET /v1/agent/dispatch?wait=60`) do not
//! have dedicated workers: their HTTP handlers (`server/routes/`)
//! send `ReducerInput` values directly into the input channel and
//! coordinate shutdown through axum-server's graceful-drain protocol,
//! not through the runtime's `ShutdownToken` constellation. Phase 6a
//! spawned stub workers as placeholders for an alternative bridge
//! architecture that never shipped; Phase 8f deleted them.

pub mod manifest_poll;
