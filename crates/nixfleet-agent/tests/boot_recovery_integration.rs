//! End-to-end boot-recovery wire test against a wiremock CP.

use chrono::Utc;
use nixfleet_agent::checkin_state::{
    self, read_last_confirmed, read_last_dispatched, write_last_dispatched, LastDispatchRecord,
};
use nixfleet_agent::recovery::run_boot_recovery;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn plain_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build plain reqwest client")
}

fn record(closure: &str) -> LastDispatchRecord {
    LastDispatchRecord {
        closure_hash: closure.to_string(),
        channel_ref: "stable@deadbeef".to_string(),
        rollout_id: Some("stable@deadbeef".to_string()),
        dispatched_at: Utc::now(),
    }
}

#[tokio::test]
async fn posted_confirm_acknowledged_clears_dispatch_writes_confirmed() {
    let dir = TempDir::new().unwrap();
    let closure = "abc-nixos-system-boot-recovery-ack";
    write_last_dispatched(dir.path(), &record(closure)).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/confirm"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    run_boot_recovery(
        &plain_client(),
        dir.path(),
        &server.uri(),
        "test-host",
        Some(closure.to_string()),
    )
    .await
    .expect("recovery returned Ok");

    assert!(
        read_last_dispatched(dir.path()).unwrap().is_none(),
        "Acknowledged confirm must clear last_dispatched",
    );
    let confirmed = read_last_confirmed(dir.path(), closure, Utc::now())
        .unwrap()
        .expect("last_confirmed_at populated post-recovery");
    let age = (Utc::now() - confirmed).num_seconds();
    assert!(
        (0..5).contains(&age),
        "last_confirmed_at should be ~now (got {age}s ago)",
    );
}

#[tokio::test]
async fn posted_confirm_410_with_failing_rollback_preserves_dispatch() {
    let dir = TempDir::new().unwrap();
    let closure = "def-nixos-system-boot-recovery-cancelled";
    write_last_dispatched(dir.path(), &record(closure)).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/confirm"))
        .respond_with(ResponseTemplate::new(410))
        .expect(1)
        .mount(&server)
        .await;

    // LOADBEARING: failed rollback must KEEP last_dispatched — clearing on failure splits brain.
    run_boot_recovery(
        &plain_client(),
        dir.path(),
        &server.uri(),
        "test-host",
        Some(closure.to_string()),
    )
    .await
    .expect("recovery returned Ok despite synthetic rollback failure");

    assert!(
        read_last_dispatched(dir.path()).unwrap().is_some(),
        "410 Cancelled with failing rollback must PRESERVE last_dispatched \
         (next-boot retry signal); clearing here would split-brain",
    );
    assert!(
        read_last_confirmed(dir.path(), closure, Utc::now())
            .unwrap()
            .is_none(),
        "410 Cancelled must NOT write last_confirmed_at",
    );
}

#[tokio::test]
async fn confirm_request_body_carries_dispatched_record_fields() {
    let dir = TempDir::new().unwrap();
    let closure = "ghi-nixos-system-shape-check";
    let rec = record(closure);
    write_last_dispatched(dir.path(), &rec).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/confirm"))
        .and(wiremock::matchers::body_partial_json(json!({
            "hostname": "shape-host",
            "rollout": "stable@deadbeef",
            "wave": 0,
            "generation": {
                "closureHash": "ghi-nixos-system-shape-check",
                "channelRef": "stable@deadbeef",
            },
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    run_boot_recovery(
        &plain_client(),
        dir.path(),
        &server.uri(),
        "shape-host",
        Some(closure.to_string()),
    )
    .await
    .expect("recovery Ok");
}

#[tokio::test]
async fn no_record_skips_post_entirely() {
    let dir = TempDir::new().unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/confirm"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    run_boot_recovery(
        &plain_client(),
        dir.path(),
        &server.uri(),
        "test-host",
        Some("any-closure".to_string()),
    )
    .await
    .expect("recovery Ok");

    let _ = checkin_state::clear_last_dispatched(dir.path());
}
