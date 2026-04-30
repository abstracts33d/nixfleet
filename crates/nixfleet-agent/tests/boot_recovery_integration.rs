//! Integration test for the ADR-011 boot-recovery PostedConfirm
//! branch.
//!
//! The unit tests in `recovery::tests` exercise the decision logic
//! against a dummy reqwest client that hits an unreachable port —
//! they prove `decide_and_run` returns the right `RecoveryAction`
//! variants but don't prove the wire round-trip works end-to-end.
//!
//! This test stands up a wiremock server, points the agent's
//! recovery hook at it with a plain HTTP client (no mTLS in the
//! test environment), pre-populates the state-dir with a
//! `last_dispatched` record matching the `current_closure` fed to
//! recovery, and asserts:
//!
//! - The recovery posts /v1/agent/confirm exactly once.
//! - The body shape matches the persisted `LastDispatchRecord`.
//! - On 204 Acknowledged, the state-dir ends up with
//!   `last_confirmed_at` written + `last_dispatched` cleared.
//! - On 410 Gone, the state-dir's `last_dispatched` is also cleared
//!   (rollback fires synthetically; we don't test the actual
//!   rollback subprocess in this integration — that's covered by
//!   harness-level scenarios).
//!
//! Wiremock supports plain HTTP only by default, which is fine for
//! the recovery contract — what we're proving is the agent-side
//! recovery flow's wire shape, not the TLS handshake.

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

    // last_dispatched cleared.
    assert!(
        read_last_dispatched(dir.path()).unwrap().is_none(),
        "Acknowledged confirm must clear last_dispatched",
    );
    // last_confirmed_at written matching the recovered closure.
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
async fn posted_confirm_410_clears_dispatch_attempts_rollback() {
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

    // run_boot_recovery on Cancelled invokes rollback() — which in
    // a unit-test environment will fail because there's no
    // `/nix/var/nix/profiles/system` to flip. The rollback's
    // failure is tracing-only (best-effort); the recovery decision
    // path still returns Ok and clears the dispatch record.
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
        read_last_dispatched(dir.path()).unwrap().is_none(),
        "410 Cancelled must clear last_dispatched",
    );
    // last_confirmed_at NOT written (rollback path doesn't confirm).
    assert!(
        read_last_confirmed(dir.path(), closure, Utc::now())
            .unwrap()
            .is_none(),
        "410 Cancelled must NOT write last_confirmed_at",
    );
}

#[tokio::test]
async fn confirm_request_body_carries_dispatched_record_fields() {
    // Stronger assertion than just "POST happened" — verify the
    // request body's fields match what we persisted, so a future
    // refactor that drops a field at the synthetic-target stage
    // surfaces here.
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
    // Empty state-dir — no last_dispatched.

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/confirm"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0) // MUST NOT be called
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

    // drop(server) at end of scope verifies the .expect(0) — the
    // Mock asserts on drop that the expected call count was met.
    let _ = checkin_state::clear_last_dispatched(dir.path());
}
