//! NixFleet agent library.
//!
//! Reads cert paths + CP URL from CLI flags (set by the NixOS
//! module), builds an mTLS reqwest client, polls
//! `/v1/agent/checkin` every `pollInterval` seconds with a richer
//! body than RFC-0003 §4.1's minimum (pending generation, last fetch
//! outcome, agent uptime). On fetch/verify failures the agent posts
//! to `/v1/agent/report`. On a dispatched target it realises and
//! activates the closure, then confirms via `/v1/agent/confirm`.
//!
//! See `rfcs/0003-protocol.md §4` and `docs/trust-root-flow.md §5`.

pub mod activation;
pub mod checkin_state;
pub mod comms;
pub mod compliance;
pub mod enrollment;
pub mod freshness;
pub mod recovery;
