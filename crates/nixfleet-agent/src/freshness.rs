//! Defense-in-depth: refuse targets whose backing manifest's `signed_at`
//! is older than the channel's freshness window when measured at dispatch
//! reception time.
//!
//! Restored from `a9ff4f43^:crates/nixfleet-agent/src/freshness.rs` (D-028)
//! after Phase 7g's "runtime is now the only loop" destructive cut removed
//! the v0.1 dispatch path that called it. In v0.2 the verify-on-fetch
//! path (`manifest_cache::fetch_or_load` → `verify_artifact`) already
//! enforces freshness against a 3600s window at fetch time; this module
//! provides the parametric pure-function form so callers can re-check
//! freshness at OTHER moments in the dispatch lifecycle (the canonical
//! example: longpoll's handle_dispatch validates the per-rollout manifest
//! is still fresh at the moment of dispatch arrival, closing the TOCTOU
//! between fetch and dispatch when the cache hit was near the freshness
//! edge).
//!
//! The v0.2 shape is parametric over `(signed_at, freshness_window_secs,
//! now)` rather than coupled to the deleted `EvaluatedTarget` type.
//! Callers extract `signed_at` from a verified manifest and the window
//! from the channel's declaration.

use chrono::{DateTime, Utc};

/// LOADBEARING: clock-skew slack added to the freshness window — without
/// it, a manifest signed `window` seconds ago is rejected the instant the
/// agent's clock is ahead of the signer's by 1s.
pub const CLOCK_SKEW_SLACK_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessCheck {
    Fresh,
    /// Caller refuses to act on the target. Common response: log + drop
    /// the dispatch; the next planner tick will re-emit (with a fresher
    /// manifest if CI has signed one).
    Stale {
        signed_at: DateTime<Utc>,
        freshness_window_secs: u32,
        age_secs: i64,
    },
}

/// Pure freshness check: compare `signed_at` against `now`, allowing the
/// channel's `freshness_window_secs` plus `CLOCK_SKEW_SLACK_SECS`. Returns
/// `Fresh` if the manifest is within the window (inclusive at the
/// boundary), `Stale` past it.
///
/// Negative ages (signer's clock ahead of agent's) are always Fresh —
/// the limit comparison is `age > limit`, so age=−30 < limit always.
pub fn check_signed_at(
    signed_at: DateTime<Utc>,
    freshness_window_secs: u32,
    now: DateTime<Utc>,
) -> FreshnessCheck {
    let age_secs = (now - signed_at).num_seconds();
    let limit = freshness_window_secs as i64 + CLOCK_SKEW_SLACK_SECS;

    if age_secs > limit {
        FreshnessCheck::Stale {
            signed_at,
            freshness_window_secs,
            age_secs,
        }
    } else {
        FreshnessCheck::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn fresh_when_age_well_under_window() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(100);
        assert_eq!(check_signed_at(signed, 3600, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn fresh_at_exact_window_boundary() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(3600);
        assert_eq!(check_signed_at(signed, 3600, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn fresh_within_slack_past_window() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(3660);
        assert_eq!(check_signed_at(signed, 3600, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn stale_just_past_slack() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(3661);
        let result = check_signed_at(signed, 3600, now);
        assert!(
            matches!(result, FreshnessCheck::Stale { age_secs: 3661, .. }),
            "expected Stale at 3661s past signed_at; got {result:?}",
        );
    }

    #[test]
    fn stale_far_past_window() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(86_400 * 3);
        let result = check_signed_at(signed, 3600, now);
        match result {
            FreshnessCheck::Stale {
                age_secs,
                freshness_window_secs,
                ..
            } => {
                assert_eq!(freshness_window_secs, 3600);
                assert_eq!(age_secs, 86_400 * 3);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn fresh_when_clock_skew_slightly_negative() {
        let signed = t0();
        let now = signed - chrono::Duration::seconds(30);
        assert_eq!(check_signed_at(signed, 3600, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn fresh_with_zero_window_inside_slack() {
        // Channel with freshness_window=0 still tolerates clock skew up to
        // CLOCK_SKEW_SLACK_SECS. Pinning this edge so future tightening of
        // the slack constant is intentional.
        let signed = t0();
        let now = signed + chrono::Duration::seconds(30);
        assert_eq!(check_signed_at(signed, 0, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn stale_with_zero_window_past_slack() {
        let signed = t0();
        let now = signed + chrono::Duration::seconds(61);
        let result = check_signed_at(signed, 0, now);
        assert!(matches!(result, FreshnessCheck::Stale { .. }));
    }
}
