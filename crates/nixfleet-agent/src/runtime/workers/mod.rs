//! Workers — the I/O-bearing edges around the agent's pure reducer.
//!
//! Each worker is spawned by [`super::spawn`] with its own
//! [`super::ShutdownToken`] and a clone of the reducer's input
//! `mpsc::Sender<ReducerInput>`. Workers never call `step()` — they
//! translate I/O into [`super::ReducerInput`] values and let the
//! reducer task do the actual transitions.

pub mod activation;
pub mod advance_ticker;
pub mod heartbeat;
pub mod longpoll;
pub mod manifest_poll;
pub mod outbound;
pub mod probe;
pub mod probe_runners;
