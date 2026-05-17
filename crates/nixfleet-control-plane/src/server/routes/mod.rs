//! Noun-based HTTP route modules.

pub(in crate::server) mod deferrals;
pub(in crate::server) mod dispatch;
pub(in crate::server) mod enrollment;
pub(in crate::server) mod events;
pub(in crate::server) mod fleet;
pub(in crate::server) mod health;
pub(in crate::server) mod heartbeat;
pub(in crate::server) mod metrics;
pub(in crate::server) mod rollouts;
pub(in crate::server) mod status;
