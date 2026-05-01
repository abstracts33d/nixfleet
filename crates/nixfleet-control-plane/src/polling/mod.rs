//! Periodic upstream polls — channel-refs source (`channel_refs_poll`),
//! revocations sidecar (`revocations_poll`), and the shared signed-fetch
//! primitive (`signed_fetch`) used by both.

pub mod channel_refs_poll;
pub mod poller;
pub mod revocations_poll;
pub mod signed_fetch;
