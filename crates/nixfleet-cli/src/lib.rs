//! Shared CLI logic — table rendering, age math, status classification.
//! Kept as a library so binaries (`nixfleet status` today, `rollout
//! trace` + `diff` next) compose against it and unit tests can exercise
//! formatting without spinning up a real CP.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use nixfleet_proto::HostStatusEntry;

pub struct StatusInputs {
    pub now: DateTime<Utc>,
    pub hosts: Vec<HostStatusEntry>,
    /// channel name → freshness_window in minutes (from `/v1/channels/{name}`).
    pub channel_freshness: BTreeMap<String, u32>,
}

pub fn render_status_table(input: &StatusInputs) -> String {
    let mut rows: Vec<[String; 6]> = Vec::with_capacity(input.hosts.len() + 1);
    rows.push([
        "HOST".into(),
        "CHANNEL".into(),
        "CURRENT".into(),
        "DECLARED".into(),
        "STATUS".into(),
        "COMPLIANCE".into(),
    ]);
    for host in &input.hosts {
        rows.push([
            host.hostname.clone(),
            host.channel.clone(),
            display_hash(host.current_closure_hash.as_deref(), "<unseen>"),
            display_hash(host.declared_closure_hash.as_deref(), "<unset>"),
            status_label(
                host,
                input.now,
                input.channel_freshness.get(&host.channel).copied(),
            ),
            compliance_label(host),
        ]);
    }

    let mut widths = [0usize; 6];
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            widths[i] = widths[i].max(col.chars().count());
        }
    }

    let mut out = String::new();
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(col);
            if i + 1 < row.len() {
                let pad = widths[i].saturating_sub(col.chars().count());
                for _ in 0..pad {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
    out
}

fn display_hash(h: Option<&str>, fallback: &str) -> String {
    match h {
        None => fallback.to_string(),
        Some(s) if s.chars().count() <= 14 => s.to_string(),
        Some(s) => {
            let prefix: String = s.chars().take(13).collect();
            format!("{prefix}\u{2026}")
        }
    }
}

fn status_label(
    host: &HostStatusEntry,
    now: DateTime<Utc>,
    freshness_minutes: Option<u32>,
) -> String {
    if host.converged {
        return "\u{2713} converged".to_string();
    }
    let Some(last) = host.last_checkin_at else {
        return "\u{2717} never".to_string();
    };
    if let Some(window) = freshness_minutes {
        let age = now.signed_duration_since(last);
        let stale_threshold = chrono::Duration::minutes(i64::from(window) * 2);
        if age > stale_threshold {
            return format!("\u{26A0} stale ({})", format_age(age));
        }
    }
    "\u{2192} in progress".to_string()
}

fn format_age(d: chrono::Duration) -> String {
    let total_seconds = d.num_seconds().max(0);
    if total_seconds >= 86400 {
        format!("{}d", total_seconds / 86400)
    } else if total_seconds >= 3600 {
        format!("{}h", total_seconds / 3600)
    } else {
        format!("{}m", total_seconds / 60)
    }
}

fn compliance_label(host: &HostStatusEntry) -> String {
    let total = host.outstanding_compliance_failures + host.outstanding_runtime_gate_errors;
    format!("{total} outstanding")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixture_host(
        hostname: &str,
        channel: &str,
        converged: bool,
        last_checkin_min_ago: Option<i64>,
        outstanding: usize,
    ) -> HostStatusEntry {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        HostStatusEntry {
            hostname: hostname.into(),
            channel: channel.into(),
            declared_closure_hash: Some("aaaaaaaaaaaaaaaaaaaa".into()),
            current_closure_hash: last_checkin_min_ago
                .map(|_| "bbbbbbbbbbbbbbbbbbbb".to_string()),
            pending_closure_hash: None,
            last_checkin_at: last_checkin_min_ago.map(|m| now - chrono::Duration::minutes(m)),
            last_rollout_id: None,
            converged,
            outstanding_compliance_failures: outstanding,
            outstanding_runtime_gate_errors: 0,
            verified_event_count: 0,
            last_uptime_secs: None,
        }
    }

    #[test]
    fn renders_three_status_classes() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let inputs = StatusInputs {
            now,
            hosts: vec![
                fixture_host("lab", "stable", true, Some(0), 0),
                fixture_host("krach", "stable", false, None, 0),
                fixture_host("ohm", "stable", false, Some(60 * 24 * 3), 2),
            ],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(out.contains("\u{2713} converged"), "no converged: {out}");
        assert!(out.contains("\u{2717} never"), "no never: {out}");
        assert!(out.contains("\u{26A0} stale (3d)"), "no stale: {out}");
        assert!(out.contains("HOST"));
        assert!(out.contains("0 outstanding"));
        assert!(out.contains("2 outstanding"));
    }

    #[test]
    fn long_hashes_truncate_with_ellipsis() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(0), 0);
        h.declared_closure_hash = Some("0123456789abcdef0123456789abcdef".into());
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::new(),
        };
        let out = render_status_table(&inputs);
        assert!(out.contains("0123456789abc\u{2026}"), "no truncation: {out}");
    }

    #[test]
    fn missing_freshness_window_skips_staleness_check() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let inputs = StatusInputs {
            now,
            hosts: vec![fixture_host("a", "stable", false, Some(60 * 24 * 7), 0)],
            channel_freshness: BTreeMap::new(),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} in progress"),
            "fell through to in-progress without a window: {out}"
        );
        assert!(!out.contains("stale"), "shouldn't be stale without window: {out}");
    }
}
