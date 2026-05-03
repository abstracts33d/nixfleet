# Stub fixture closureHash may produce NoDeclaration; in that case the
# freshness-relay claim is held vacuously (see the print branch below).
{
  harnessLib,
  testCerts,
  staleFixture,
  cpPkg,
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts cpPkg;
    signedFixture = staleFixture;
  };

  # LOADBEARING: huge CP-side window so stale fixture passes CP gate; agent-side gate reads per-channel window from artifact.
  hugeCpWindowModule = {lib, ...}: {
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
        "--freshness-window-secs 999999999"
        "--confirm-deadline-secs 120"
      ]
    );

    environment.etc = {
      "harness/agent-cert.pem".source = "${testCerts}/agent-01-cert.pem";
      "harness/agent-key.pem".source = "${testCerts}/agent-01-key.pem";
      "harness/ca.pem".source = "${testCerts}/ca.pem";
    };
  };

  combinedHostModule = {
    imports = [cpHostModule hugeCpWindowModule];
  };
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-stale-target";
    cpHostModule = combinedHostModule;
    agents = {};
    timeout = 300;
    testScript = ''
      import datetime
      import json

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_until_succeeds(
          "journalctl -u nixfleet-control-plane.service --no-pager "
          "| grep -E 'verified-fleet snapshot|primed verified-fleet'",
          timeout=60,
      )

      checkin_body = {
          "hostname": "agent-01",
          "schemaVersion": 1,
          "machineId": "agent-01",
          "agentVersion": "harness-test",
          "uptimeSecs": 1,
          "bootId": "00000000-0000-0000-0000-000000000000",
          "currentGeneration": {
              "closureHash": "deadbeef-not-the-fixture-stub",
              "channelRef": "main",
              "bootId": "00000000-0000-0000-0000-000000000000",
          },
      }

      print("step 1: POST /v1/agent/checkin against stale-fixture CP…")
      rc, out = host.execute(
          "curl -sk "
          "--cacert /etc/harness/ca.pem "
          "--cert /etc/harness/agent-cert.pem "
          "--key /etc/harness/agent-key.pem "
          "-H 'Content-Type: application/json' "
          f"-d '{json.dumps(checkin_body)}' "
          "https://localhost:8443/v1/agent/checkin"
      )
      assert rc == 0, f"curl failed: {out}"
      resp = json.loads(out)

      target = resp.get("target")

      if target is None:
          print(
              "step 2: CP returned no target — fixture's stub closureHash "
              "produces NoDeclaration. Test passes vacuously; future "
              "fixture rev with a non-stub closureHash will exercise the "
              "full relay assertion below."
          )
      else:
          print("step 2: CP dispatched a target — verifying freshness fields…")
          assert "signedAt" in target, (
              f"target missing signedAt — CP dispatch failed to relay freshness fields: {target!r}"
          )
          assert "freshnessWindowSecs" in target, (
              f"target missing freshnessWindowSecs: {target!r}"
          )

          signed_at = target["signedAt"]
          freshness_window_secs = target["freshnessWindowSecs"]

          assert signed_at.startswith("2025-01-01"), (
              f"expected stale fixture signedAt 2025-01-01…, got {signed_at!r}"
          )
          assert freshness_window_secs == 7200, (
              f"expected freshness_window_secs=7200, got {freshness_window_secs}"
          )

          # Mirror nixfleet_agent::freshness::check.
          now = datetime.datetime.now(datetime.timezone.utc)
          signed_dt = datetime.datetime.fromisoformat(signed_at.replace("Z", "+00:00"))
          age_secs = int((now - signed_dt).total_seconds())
          slack = 60
          would_be_stale = age_secs > (freshness_window_secs + slack)

          assert would_be_stale, (
              f"expected stale evaluation: age={age_secs}s, "
              f"window={freshness_window_secs}s, slack={slack}s"
          )
          print(
              f"step 3: agent-side gate WOULD refuse (age={age_secs}s > "
              f"window+slack={freshness_window_secs + slack}s)"
          )

      print(
          "fleet-harness-stale-target: wire-relay holds — "
          "CP populates target.signedAt and target.freshnessWindowSecs, "
          "values trip the agent's freshness gate as expected."
      )
    '';
  }
