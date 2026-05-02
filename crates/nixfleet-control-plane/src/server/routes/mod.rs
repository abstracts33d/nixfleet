//! Noun-based HTTP route modules. Each module owns the handlers
//! for one URL noun:
//!
//! - `enrollment` — `/v1/enroll` + `/v1/agent/renew`
//! - `reports`    — `/v1/agent/report`
//! - `rollouts`   — `/v1/rollouts/*`
//! - `status`     — `/v1/whoami`, `/v1/channels/*`, `/v1/hosts`,
//!                  `/v1/agent/closure/*`
//! - `health`     — `/healthz`
//!
//! The check-in pipeline lives in the sibling `checkin_pipeline/`
//! module (it's a multi-stage decision pipeline, not a single
//! handler).

pub(in crate::server) mod enrollment;
pub(in crate::server) mod health;
pub(in crate::server) mod reports;
pub(in crate::server) mod rollouts;
pub(in crate::server) mod status;
