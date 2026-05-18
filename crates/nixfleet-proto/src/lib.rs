#![allow(clippy::doc_lazy_continuation)]
//! Boundary-contract types. Optional fields serialize `null` (not omitted)
//! to match the Nix evaluator's shape so JCS bytes round-trip identically.

pub mod agent_event;
pub mod agent_wire;
pub mod bootstrap_nonces;
pub mod clock;
pub mod enroll_wire;
pub mod evidence;
pub mod evidence_signing;
pub mod fleet_resolved;
pub mod fleet_view;
pub mod host_key;
pub mod host_rollout_state;
pub mod revocations;
pub mod rollout_manifest;
pub mod trust;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// A signed channel ref (typically a closure hash or a tagged ref name).
/// Shared between reconciler (`PlanAction::OpenRollout.target_ref`) and
/// state-machine (`RolloutEvent::RolloutOpened.target_ref`); lives in proto
/// so both pure crates can depend on it without cross-edges (RFC-0012 §7).
pub type ChannelRef = String;

/// Content-addressed rollout identifier (RFC-0012 §6.3).
///
/// Identity contract: a `RolloutId` is the canonical
/// `"{channel}@{channel_ref}"` composite. Constructed only via
/// [`RolloutId::new`]; the private inner field prevents ad-hoc
/// construction the same way `Verified<T>` prevents ad-hoc construction
/// of a "verified" payload (RFC-0009 §3). Test code that needs to forge
/// a pre-formed identifier goes through `from_raw_for_tests` under the
/// `test-helpers` feature gate.
///
/// Rationale (RFC-0012 §6.3): two channels can share a `channel_ref`
/// (the architectural point of multi-channel cascading from a single
/// git push). `channel_ref` alone collides in that topology;
/// `channel` alone violates the content-addressed property of the rest
/// of the cycle. The composite preserves both: unique per
/// `(channel, channel_ref)` AND deterministic across replays.
///
/// SQL bindings: call `.as_str()` on the field at the `params![...]`
/// site. Production code MUST NOT round-trip raw strings into
/// `from_raw_for_tests` — the type-level enforcement is the contract.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RolloutId(String);

impl RolloutId {
    /// Construct the canonical `"{channel}@{channel_ref}"` identifier.
    /// The only non-test path to a `RolloutId`.
    ///
    /// `channel` MUST NOT contain `@`; the parser splits on the FIRST
    /// `@` boundary (per `channel()` / `channel_ref()`), so an `@` in
    /// `channel` would break round-trip identity. The wire-level route
    /// validator (CP `looks_like_rollout_id`) locks `channel` to
    /// `[a-z0-9_-]+` already; the `debug_assert!` pins the same
    /// invariant at the type boundary so a wire-bypass round-trip
    /// fails fast in development.
    pub fn new(channel: &str, channel_ref: &str) -> Self {
        debug_assert!(
            !channel.contains('@'),
            "channel must not contain '@' (per RFC-0012 §6.3 canonical format)",
        );
        Self(format!("{channel}@{channel_ref}"))
    }

    /// String view for SQL bindings + logging. Cheap (no allocation).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Channel half of the canonical composite (per RFC-0012 §6.3).
    /// Splits on the first `@`; the constructor's invariant guarantees
    /// the channel never carries `@` itself, so this returns the
    /// operator-declared channel name unmodified.
    pub fn channel(&self) -> &str {
        self.0.split_once('@').map(|(c, _)| c).unwrap_or(&self.0)
    }

    /// Channel-ref half of the canonical composite (per RFC-0012 §6.3).
    /// Splits on the first `@`; if no `@` is present (only reachable via
    /// `from_raw_for_tests` with a malformed input), returns the empty
    /// string rather than panic.
    pub fn channel_ref(&self) -> &str {
        self.0.split_once('@').map(|(_, r)| r).unwrap_or("")
    }

    /// **TEST ONLY.** Bypasses the canonical-format constructor. Available
    /// only when the crate is compiled with `#[cfg(test)]` (proto's own
    /// lib tests) or with the `test-helpers` feature flag (downstream
    /// crates' tests, enabled via `[dev-dependencies]` so production
    /// builds cannot reach it). Same convention as
    /// `nixfleet_reconciler::Verified::unverified_for_tests`.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn from_raw_for_tests(raw: String) -> Self {
        Self(raw)
    }
}

/// **TEST ONLY.** Ergonomic `"rid".into()` construction for test
/// fixtures. Same `test-helpers` gate as `from_raw_for_tests`;
/// production code constructs via `RolloutId::new(channel, channel_ref)`.
#[cfg(any(test, feature = "test-helpers"))]
impl From<&str> for RolloutId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl From<String> for RolloutId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for RolloutId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RolloutId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for RolloutId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for RolloutId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        // Reads round-trip through the canonical-format constraint at
        // the storage layer (RFC-0012 §6.3): rollout_id is written via
        // `RolloutId::new`'s output and read back unchanged. The DB
        // boundary is the one legitimate non-`new` construction path;
        // every other path goes through `new` or `from_raw_for_tests`.
        <String as rusqlite::types::FromSql>::column_result(value).map(Self)
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for RolloutId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

pub use agent_event::{
    AgentEvent, AgentEventEnvelope, OnHealthFailureWire, ProbeModeWire, ProbeStatusWire,
    ProbeSubResultWire, ProbeTopologyEntryWire,
};
pub use bootstrap_nonces::{BootstrapNonceEntry, BootstrapNonces};
pub use clock::{Clock, ClockHandle, FakeClock, SystemClock};
pub use fleet_resolved::{
    Channel, ChannelEdge, DisruptionBudget, Edge, FleetResolved, HealthGate, Host, Meta,
    OnHealthFailure, Pin, PolicyWave, RolloutPolicy, STRATEGY_ALL_AT_ONCE, Selector,
    SystemdFailedUnits, Wave, normalize_rollout_policies,
};
pub use fleet_view::{
    HostStatusEntry, HostsResponse, RolloutEventEntry, RolloutEvents, RolloutHostEntry,
    RolloutHosts,
};
pub use host_rollout_state::HostRolloutState;
pub use revocations::{RevocationEntry, Revocations};
pub use rollout_manifest::{HostWave, RolloutBudget, RolloutManifest};
pub use trust::{KeySlot, TrustConfig, TrustedPubkey};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_accessor_returns_left_of_at() {
        let id = RolloutId::new("stable", "abc1234");
        assert_eq!(id.channel(), "stable");
    }

    #[test]
    fn channel_ref_accessor_returns_right_of_at() {
        let id = RolloutId::new("stable", "abc1234");
        assert_eq!(id.channel_ref(), "abc1234");
    }

    #[test]
    fn accessors_split_on_first_at_only() {
        // Constructor's debug_assert prevents `@` in `channel`, but a
        // `channel_ref` containing `@` (reachable only via
        // `from_raw_for_tests` since the route validator restricts
        // channel_ref to lowercase hex) parses correctly because
        // split_once splits on the FIRST `@` only. Documents the parser's
        // semantics so a future change to split_once's behaviour fails
        // this test loudly.
        let weird = RolloutId::from_raw_for_tests("a@b@c".to_string());
        assert_eq!(weird.channel(), "a");
        assert_eq!(weird.channel_ref(), "b@c");
    }
}
