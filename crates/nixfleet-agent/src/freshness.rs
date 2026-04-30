//! Defense-in-depth: refuse any dispatched target whose backing
//! `fleet.resolved.json` is older than the channel's
//! `freshness_window`. CP applies the same gate at tick start.
//! Seeing a stale target normally indicates clock skew or a CP gate
//! that failed open.

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::EvaluatedTarget;

pub const CLOCK_SKEW_SLACK_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessCheck {
    Fresh,
    /// Agent must refuse activation and post `StaleTarget`.
    Stale {
        signed_at: DateTime<Utc>,
        freshness_window_secs: u32,
        age_secs: i64,
    },
    /// Older CP didn't relay enough info — fail open with warn.
    Unknown,
}

pub fn check(target: &EvaluatedTarget, now: DateTime<Utc>) -> FreshnessCheck {
    let (signed_at, window_secs) = match (target.signed_at, target.freshness_window_secs) {
        (Some(s), Some(w)) => (s, w),
        _ => return FreshnessCheck::Unknown,
    };

    let age_secs = (now - signed_at).num_seconds();
    let limit = window_secs as i64 + CLOCK_SKEW_SLACK_SECS;

    if age_secs > limit {
        FreshnessCheck::Stale {
            signed_at,
            freshness_window_secs: window_secs,
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

    fn target_with(signed_at: Option<DateTime<Utc>>, window: Option<u32>) -> EvaluatedTarget {
        EvaluatedTarget {
            closure_hash: "h".into(),
            channel_ref: "stable@abc".into(),
            evaluated_at: Utc::now(),
            rollout_id: None,
            wave_index: None,
            activate: None,
            signed_at,
            freshness_window_secs: window,
            compliance_mode: None,
        }
    }

    #[test]
    fn unknown_when_signed_at_missing() {
        let t = target_with(None, Some(3600));
        assert_eq!(check(&t, Utc::now()), FreshnessCheck::Unknown);
    }

    #[test]
    fn unknown_when_window_missing() {
        let t = target_with(Some(Utc::now()), None);
        assert_eq!(check(&t, Utc::now()), FreshnessCheck::Unknown);
    }

    #[test]
    fn fresh_when_age_well_under_window() {
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed + chrono::Duration::seconds(100);
        let t = target_with(Some(signed), Some(3600));
        assert_eq!(check(&t, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn fresh_at_exact_window_boundary() {
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed + chrono::Duration::seconds(3600);
        let t = target_with(Some(signed), Some(3600));
        assert_eq!(check(&t, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn fresh_within_slack_past_window() {
        // 60s slack means age=window+60 is still fresh.
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed + chrono::Duration::seconds(3660);
        let t = target_with(Some(signed), Some(3600));
        assert_eq!(check(&t, now), FreshnessCheck::Fresh);
    }

    #[test]
    fn stale_just_past_slack() {
        // age=window+61 → stale.
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed + chrono::Duration::seconds(3661);
        let t = target_with(Some(signed), Some(3600));
        assert!(matches!(
            check(&t, now),
            FreshnessCheck::Stale { age_secs: 3661, .. }
        ));
    }

    #[test]
    fn stale_far_past_window() {
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed + chrono::Duration::seconds(86_400 * 3);
        let t = target_with(Some(signed), Some(3600));
        let result = check(&t, now);
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
        // Agent clock 30s behind signing clock — age is "negative",
        // never trips the freshness gate.
        let signed = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let now = signed - chrono::Duration::seconds(30);
        let t = target_with(Some(signed), Some(3600));
        assert_eq!(check(&t, now), FreshnessCheck::Fresh);
    }
}
