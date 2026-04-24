//! Per-host state-machine transitions (RFC-0002 §3.2).

#[path = "common/mod.rs"]
mod common;

#[test]
fn queued_to_dispatched() {
    let (actual, expected) = common::run("host/queued_to_dispatched");
    common::assert_matches(&actual, &expected);
}

#[test]
fn healthy_to_soaked() {
    let (actual, expected) = common::run("host/healthy_to_soaked");
    common::assert_matches(&actual, &expected);
}

#[test]
fn confirmwindow_blocks_wave() {
    let (actual, expected) = common::run("host/confirmwindow_timeout_reverted");
    common::assert_matches(&actual, &expected);
}

#[test]
fn host_failed_triggers_halt() {
    let (actual, expected) = common::run("host/host_failed_triggers_halt");
    common::assert_matches(&actual, &expected);
}

#[test]
fn offline_host_skipped() {
    let (actual, expected) = common::run("host/offline_host_skipped");
    common::assert_matches(&actual, &expected);
}
