# Secret-hygiene demonstration. ARCHITECTURE.md §8: a CP whose disk
# is stolen in its entirety must yield zero plaintext secret material.
# The agent decrypts an age-encrypted blob at boot, lands the
# plaintext in /run/secrets/test-token, then talks to the CP
# normally. testScript dumps every CP-resident artifact (SQLite,
# journal, audit.log, observed.json, etc files) and grep-asserts the
# plaintext does NOT appear.
#
# Regression-catcher framing: under code construction the agent
# never reads /run/secrets, so no plaintext can travel. This
# scenario fails loudly the first time someone wires a debug log
# that dumps secret material into a CP-bound request.
{
  pkgs,
  lib,
  harnessLib,
  testCerts,
  signedFixture,
  agenixFixture,
  cpPkg,
  agentPkg,
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Extra NixOS module that lands on the agent microVM. Stages the
  # encrypted blob + age identity at fixed paths and runs a one-shot
  # decrypt unit before the agent service boots so the plaintext is
  # in place during the agent's poll loop.
  decryptModule = {pkgs, ...}: {
    environment.etc = {
      "harness-secret/identity.txt".source = "${agenixFixture}/identity.txt";
      "harness-secret/secret.age".source = "${agenixFixture}/secret.age";
    };
    systemd.tmpfiles.rules = [
      "d /run/secrets 0700 root root -"
    ];
    systemd.services.decrypt-test-secret = {
      description = "Decrypt the harness test secret into /run/secrets";
      wantedBy = ["multi-user.target"];
      before = ["nixfleet-agent.service"];
      after = ["local-fs.target"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = pkgs.writeShellScript "decrypt-test-secret" ''
          set -euo pipefail
          ${pkgs.age}/bin/age -d \
            -i /etc/harness-secret/identity.txt \
            -o /run/secrets/test-token \
            /etc/harness-secret/secret.age
          chmod 600 /run/secrets/test-token
        '';
      };
    };
  };

  agent = harnessLib.mkRealAgentNode {
    inherit testCerts signedFixture agentPkg;
    hostName = "agent-01";
    pollIntervalSecs = 10;
    extraModules = [decryptModule];
  };
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-secret-hygiene";
    inherit cpHostModule;
    agents = {agent-01 = agent;};
    timeout = 600;
    testScript = ''
      import time

      # The plaintext bytes the test must NOT find anywhere on the
      # CP host. Sourced from the agenix fixture so the value stays
      # in sync with what the agent actually decrypts.
      PLAINTEXT = open("${agenixFixture}/plaintext.txt").read()
      assert PLAINTEXT, "agenix fixture plaintext is empty"

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_for_unit("microvms.target", timeout=300)
      host.wait_for_unit("microvm@agent-01.service", timeout=300)

      # Verify the agent actually decrypted the secret — sanity check
      # that the test setup is exercising what it claims to.
      agent_01.wait_for_unit("decrypt-test-secret.service")
      agent_01.succeed("test -s /run/secrets/test-token")
      decrypted = agent_01.succeed("cat /run/secrets/test-token")
      assert decrypted == PLAINTEXT, (
          "agent did not decrypt the expected plaintext "
          f"(got {len(decrypted)} bytes; expected {len(PLAINTEXT)})"
      )

      # Wait for a handful of checkins to land so the CP journal +
      # SQLite have meaningful traffic to grep through.
      print("waiting 45s for checkin traffic to accumulate…")
      time.sleep(45)

      # Drop the plaintext into a file on the host VM so we can grep
      # everything in one shot via grep -f rather than shell-quoting
      # the plaintext into each command.
      host.succeed(f"printf '%s' {PLAINTEXT!r} > /tmp/plaintext.needle")

      # Targets to inspect: SQLite DB, runtime journal, audit log,
      # observed.json, and every staged config file under /etc
      # nixfleet-cp/.
      checks = [
          ("CP state.db", "cat /var/lib/nixfleet-cp/state.db 2>/dev/null"),
          ("CP audit.log", "cat /var/lib/nixfleet-cp/audit.log 2>/dev/null"),
          ("CP journal", "journalctl -u nixfleet-control-plane.service --no-pager"),
          ("CP /etc tree", "find /etc/nixfleet-cp -type f -exec cat {} +"),
      ]
      leaks = []
      for label, cmd in checks:
          rc, _ = host.execute(
              f"{cmd} | grep -aFq -f /tmp/plaintext.needle"
          )
          if rc == 0:
              leaks.append(label)
      if leaks:
          raise Exception(
              f"plaintext secret leaked into CP-resident state: {leaks}"
          )

      print(
          "fleet-harness-secret-hygiene: CP disk + journal contain "
          "zero bytes of the agent-side plaintext (ARCHITECTURE.md §8)."
      )
    '';
  }
