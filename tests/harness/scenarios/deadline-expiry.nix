# tests/harness/scenarios/deadline-expiry.nix
#
# Issue #2 step 5 deadline-expiry scenario.
#
# Validates the magic-rollback CP-side path: when an agent's confirm
# deadline expires before /v1/agent/confirm lands, the rollback_timer
# marks the pending_confirms row as `rolled-back` and any future
# confirm POST against that row receives HTTP 410 Gone — which the
# agent maps to `ConfirmOutcome::Cancelled` and triggers a local
# rollback (see crates/nixfleet-agent/src/main.rs:285-305).
#
# Sequence:
#   1. Host VM boots cp-real with --confirm-deadline-secs 3 (much
#      smaller than production's 120s default so the deadline window
#      is observable inside a test budget).
#   2. testScript waits for CP up, then directly INSERTs a
#      pending_confirms row into /var/lib/nixfleet-cp/state.db with
#      deadline 30s in the past (mimics what dispatch would emit on
#      a real checkin, minus the dispatch round-trip itself).
#   3. testScript POSTs ConfirmRequest via mTLS curl using the
#      pre-minted agent-01 cert. Expects HTTP 410.
#   4. testScript asserts the row state in the DB has flipped to
#      'rolled_back' (the rollback_timer or the handler itself
#      transitioning the row counts as a pass).
#
# Why no agent microVM:
#   The wire behavior under test is fully CP-side. Spinning up a
#   real-binary agent microVM that activates a stub closure would
#   add boot time, fail at the realise step (the fixture's
#   closure_hash is the deterministic stub `0000…0000`), and not
#   exercise the deadline path — the agent would post
#   ActivationFailed long before any /confirm. Using host-side curl
#   keeps the test focused on the 410 contract.
#
# Why deadline=now-30s instead of deadline=now+1s + sleep:
#   handlers.rs `record_pending_confirm` is the only caller in
#   production code; tests bypass it via direct INSERT to set up
#   exactly the state we want. Making the deadline already-past
#   means the handler's UPDATE-with-WHERE-clause finds 0 rows
#   matching `state='pending' AND deadline > now`, returns 410, and
#   we assert that. Removes any timing flakiness from the test.
{
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Override the CP unit's --confirm-deadline-secs flag to 3 so the
  # test doesn't depend on the production default. The cp-real node
  # builds the ExecStart from a list; we patch it via mkOverride.
  shortDeadlineModule = {lib, ...}: {
    systemd.services.nixfleet-control-plane.serviceConfig.ExecStart = lib.mkForce (
      lib.concatStringsSep " " [
        "${cpPkg}/bin/nixfleet-control-plane"
        "serve"
        "--listen 0.0.0.0:8443"
        "--tls-cert /etc/nixfleet-cp/cp-cert.pem"
        "--tls-key /etc/nixfleet-cp/cp-key.pem"
        "--client-ca /etc/nixfleet-cp/ca.pem"
        "--fleet-ca-cert /etc/nixfleet-cp/fleet-ca-cert.pem"
        "--fleet-ca-key /etc/nixfleet-cp/fleet-ca-key.pem"
        "--audit-log /var/lib/nixfleet-cp/audit.log"
        "--artifact /etc/nixfleet-cp/canonical.json"
        "--signature /etc/nixfleet-cp/canonical.json.sig"
        "--trust-file /etc/nixfleet-cp/test-trust.json"
        "--observed /etc/nixfleet-cp/observed.json"
        "--db-path /var/lib/nixfleet-cp/state.db"
        "--freshness-window-secs 604800"
        "--confirm-deadline-secs 3"
      ]
    );

    # Mount the agent-01 client cert + key on the host VM so the
    # testScript can curl with mTLS without spinning up a microVM.
    environment.etc = {
      "harness/agent-cert.pem".source = "${testCerts}/agent-01-cert.pem";
      "harness/agent-key.pem".source = "${testCerts}/agent-01-key.pem";
      "harness/ca.pem".source = "${testCerts}/ca.pem";
    };
  };

  combinedHostModule = {
    imports = [cpHostModule shortDeadlineModule];
  };
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-deadline-expiry";
    cpHostModule = combinedHostModule;
    agents = {}; # no agent microVMs — wire flow driven by host-side curl
    timeout = 300;
    testScript = ''
      import json
      import subprocess

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      # Step 1: inject a pending_confirms row with deadline 30s in
      # the past. Bypasses dispatch (no real agent in the loop) but
      # produces exactly the DB state the rollback_timer + /confirm
      # handler are designed to detect.
      print("step 1: inject expired pending_confirms row…")
      host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db \"\""
          "INSERT INTO pending_confirms ("
          "  hostname, rollout_id, wave, closure_hash, channel_ref,"
          "  state, dispatched_at, deadline_at"
          ") VALUES ("
          "  'agent-01', 'stable@expired1', 0,"
          "  'deadbeef-stub-closure', 'main',"
          "  'pending',"
          "  datetime('now', '-60 seconds'),"
          "  datetime('now', '-30 seconds')"
          ");"
          "\""
      )

      # Sanity: row visible, state=pending.
      pre_state = host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db "
          "\"SELECT state FROM pending_confirms WHERE rollout_id='stable@expired1';\""
      ).strip()
      assert pre_state == "pending", f"expected pending pre-confirm, got {pre_state!r}"

      # Step 2: POST /v1/agent/confirm with mTLS. Expect HTTP 410
      # because the deadline has passed.
      print("step 2: POST /v1/agent/confirm against expired row…")
      confirm_body = {
          "hostname": "agent-01",
          "rollout": "stable@expired1",
          "wave": 0,
          "generation": {
              "closureHash": "deadbeef-stub-closure",
              "channelRef": "main",
              "bootId": "00000000-0000-0000-0000-000000000000",
          },
      }

      rc, out = host.execute(
          "curl -sk -o /dev/null -w '%{http_code}' "
          "--cacert /etc/harness/ca.pem "
          "--cert /etc/harness/agent-cert.pem "
          "--key /etc/harness/agent-key.pem "
          "-H 'Content-Type: application/json' "
          f"-d '{json.dumps(confirm_body)}' "
          "https://localhost:8443/v1/agent/confirm"
      )
      assert rc == 0, f"curl failed: {out}"
      assert out.strip() == "410", (
          f"expected HTTP 410 for expired-deadline confirm, got {out.strip()!r}"
      )
      print("step 2: 410 received as expected")

      # Step 3: row state should be `rolled_back` after the handler
      # transitions it (or the rollback_timer's 30s tick — whichever
      # fires first; in practice the handler itself updates the row
      # when it returns 410 from /confirm).
      print("step 3: assert row marked rolled_back…")
      post_state = host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db "
          "\"SELECT state FROM pending_confirms WHERE rollout_id='stable@expired1';\""
      ).strip()
      assert post_state in ("rolled_back", "rolled-back"), (
          f"expected rolled_back state after 410, got {post_state!r}"
      )

      print(
          "fleet-harness-deadline-expiry: issue #2 step 5 contract holds — "
          "expired pending_confirms returns 410, row transitions to rolled_back."
      )
    '';
  }
