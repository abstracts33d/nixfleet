//! Agent ↔ control-plane wire types. LOADBEARING: within a major version,
//! additions must be backwards-compatible (older consumers serde-ignore unknown
//! fields); bump `PROTOCOL_MAJOR_VERSION` for any breaking change.
//!
//! Phase 8d trimmed the legacy v0.1 checkin / confirm / activate wire shape;
//! Phase 9a then deleted the legacy `/v1/agent/report` surface (`ReportRequest`,
//! `ReportEvent`, `ReportResponse`) — the unified event-driven wire (RFC-0008
//! §4.2 → `runtime/wire.rs` + CP's `server/routes/events.rs`) is now the
//! sole agent→CP event channel.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

/// Sent in `X-Nixfleet-Protocol`; CP rejects mismatched majors with 426.
pub const PROTOCOL_MAJOR_VERSION: u32 = 1;

pub const PROTOCOL_VERSION_HEADER: &str = "x-nixfleet-protocol";

#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRef {
    pub closure_hash: String,
    #[serde(default)]
    pub channel_ref: Option<String>,
    pub boot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGeneration {
    pub closure_hash: String,
}
