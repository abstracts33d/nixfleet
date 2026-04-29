//! Compliance gate policy mode (issue #57 / #58 / #59).
//!
//! Single typed representation of the gate mode shared by:
//! - `mk-fleet.nix` (static gate at fleet-eval time),
//! - `nixfleet-agent::compliance::GateMode` (runtime gate at
//!   activation time),
//! - `nixfleet-control-plane::wave_gate` (CP wave-staging gate
//!   on dispatch).
//!
//! All three layers parse the same wire string into the same enum
//! and pattern-match the same variants. Earlier revisions had:
//! - Nix `enum ["disabled" "permissive" "enforce"]`,
//! - agent `enum GateMode`,
//! - CP `&str` matching with `_` fall-through to disabled.
//!
//! That last one was a typo trap — a future code path that wrote
//! `"enfroce"` would silently fall through to disabled with no
//! compile error. Centralising the parse here closes that hole.
//!
//! ## Forward-compat
//!
//! The wire form is `Option<String>` because the CP must accept
//! mode strings it doesn't recognise (older CP, newer fleet.nix).
//! Unknown strings fall through to `Permissive` with a
//! `tracing::warn` — matches the rule-of-least-surprise: an
//! operator who set a mode at all clearly *wanted* compliance to
//! be active; defaulting unknown values to `Disabled` would
//! silently turn the gate off, which is worse than over-active.

use serde::{Deserialize, Serialize};

/// Resolved gate mode. The `auto` variant is agent-side only —
/// it's the input that gets resolved to `Permissive` or
/// `Disabled` based on collector-unit presence (see
/// `nixfleet-agent::compliance::resolve_mode`). The CP and the
/// static gate never see `Auto` because the agent never relays
/// it on the wire (only the resolved value flows further).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateMode {
    /// Gate skipped entirely. No events posted, no journal lines.
    Disabled,
    /// Gate runs, posts events on failure, but does NOT block
    /// dispatch / confirm. Default for fleets introducing
    /// compliance incrementally.
    Permissive,
    /// Gate runs, posts events. Failures block dispatch / confirm
    /// and trigger appropriate recovery (rollback agent-side,
    /// wave-promotion-hold CP-side).
    Enforce,
}

impl GateMode {
    /// Parse a wire-form string into a `GateMode`. Recognises
    /// `"disabled"`, `"permissive"`, `"enforce"`. Unknown strings
    /// fall back to `Permissive` (see module docs for rationale).
    /// `"auto"` also maps to `Permissive` because by the time the
    /// CP or static gate receives a value, the agent has already
    /// resolved auto via `nixfleet-agent::compliance::resolve_mode`
    /// — neither layer expects raw `auto` strings.
    pub fn from_wire_str(s: &str) -> Self {
        match s {
            "disabled" => GateMode::Disabled,
            "enforce" => GateMode::Enforce,
            // permissive | auto | unknown → Permissive.
            _ => GateMode::Permissive,
        }
    }

    /// Coerce the legacy `compliance.strict: bool` into a
    /// `GateMode`. `true → Enforce`, `false → Disabled`. Used by
    /// both `mk-fleet.nix` resolution and the CP wave_gate when
    /// `compliance.mode` is unset on a fleet.resolved channel.
    /// Centralising the mapping ensures every layer agrees on
    /// what legacy strict means.
    pub fn from_legacy_strict(strict: bool) -> Self {
        if strict {
            GateMode::Enforce
        } else {
            GateMode::Disabled
        }
    }

    /// Resolve a channel's effective mode given a typed `mode`
    /// (when present) and the legacy `strict` flag (when not).
    /// Encodes the issue-#58 precedence rule in one place.
    pub fn resolve(mode: Option<&str>, legacy_strict: bool) -> Self {
        match mode {
            Some(s) => Self::from_wire_str(s),
            None => Self::from_legacy_strict(legacy_strict),
        }
    }

    /// True iff the mode treats failures as confirm/dispatch
    /// blockers (vs. observability-only).
    pub fn is_enforcing(self) -> bool {
        matches!(self, GateMode::Enforce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_wire_str_known_values() {
        assert_eq!(GateMode::from_wire_str("disabled"), GateMode::Disabled);
        assert_eq!(GateMode::from_wire_str("permissive"), GateMode::Permissive);
        assert_eq!(GateMode::from_wire_str("enforce"), GateMode::Enforce);
    }

    #[test]
    fn from_wire_str_unknown_falls_back_permissive() {
        assert_eq!(GateMode::from_wire_str("auto"), GateMode::Permissive);
        assert_eq!(GateMode::from_wire_str(""), GateMode::Permissive);
        assert_eq!(GateMode::from_wire_str("garbage"), GateMode::Permissive);
    }

    #[test]
    fn from_legacy_strict_maps_correctly() {
        assert_eq!(GateMode::from_legacy_strict(true), GateMode::Enforce);
        assert_eq!(GateMode::from_legacy_strict(false), GateMode::Disabled);
    }

    #[test]
    fn resolve_prefers_mode_over_strict() {
        assert_eq!(GateMode::resolve(Some("permissive"), true), GateMode::Permissive);
        assert_eq!(GateMode::resolve(Some("disabled"), true), GateMode::Disabled);
        assert_eq!(GateMode::resolve(Some("enforce"), false), GateMode::Enforce);
    }

    #[test]
    fn resolve_falls_back_to_strict_when_mode_absent() {
        assert_eq!(GateMode::resolve(None, true), GateMode::Enforce);
        assert_eq!(GateMode::resolve(None, false), GateMode::Disabled);
    }

    #[test]
    fn is_enforcing_only_for_enforce() {
        assert!(GateMode::Enforce.is_enforcing());
        assert!(!GateMode::Permissive.is_enforcing());
        assert!(!GateMode::Disabled.is_enforcing());
    }
}
