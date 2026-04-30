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
  #
  # The unit emits `harness-decrypt-ok: bytes=N` on success — a
  # non-leaky proof that the decrypt happened, and the byte count
  # lets the testScript sanity-check against the fixture without
  # ever shipping the plaintext through the host journal.
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
        StandardOutput = "journal+console";
        StandardError = "journal+console";
        ExecStart = pkgs.writeShellScript "decrypt-test-secret" ''
          set -euo pipefail
          ${pkgs.age}/bin/age -d \
            -i /etc/harness-secret/identity.txt \
            -o /run/secrets/test-token \
            /etc/harness-secret/secret.age
          chmod 600 /run/secrets/test-token
          bytes=$(${pkgs.coreutils}/bin/stat -c %s /run/secrets/test-token)
          # Marker carries byte count, never plaintext. Surfaces in
          # the host journal via the microvm guest's console pipe.
          echo "harness-decrypt-ok: bytes=$bytes"
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
      import re
      import time

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_for_unit("microvms.target", timeout=300)
      host.wait_for_unit("microvm@agent-01.service", timeout=300)

      # Wait for the decrypt unit's success marker to surface in the
      # microvm's console-forwarded journal. Proves the agent VM
      # actually ran the decrypt — not just that the unit was
      # configured.
      decrypt_re = re.compile(r"harness-decrypt-ok: bytes=(\d+)")
      deadline = time.monotonic() + 60
      match = None
      while time.monotonic() < deadline:
          rc, out = host.execute(
              "journalctl -u microvm@agent-01.service --no-pager"
          )
          if rc == 0:
              m = decrypt_re.search(out)
              if m:
                  match = m
                  break
          time.sleep(2)
      if match is None:
          raise Exception("agent did not emit harness-decrypt-ok marker within 60s")

      decrypted_bytes = int(match.group(1))
      expected_bytes = int(host.succeed(
          "stat -c %s ${agenixFixture}/plaintext.txt"
      ).strip())
      assert decrypted_bytes == expected_bytes, (
          f"decrypt produced {decrypted_bytes} bytes; "
          f"fixture plaintext is {expected_bytes} bytes"
      )
      print(f"decrypt unit landed {decrypted_bytes}-byte plaintext on agent")

      # Let checkin traffic accumulate so CP-side journal + SQLite
      # have meaningful content to grep through.
      print("waiting 45s for checkin traffic to accumulate…")
      time.sleep(45)

      # Inspect every CP-resident artifact. The fixture's plaintext
      # path is in the host's nix store (build input of this test);
      # `grep -aFf` reads the needle from the file directly so we
      # never ship the plaintext through the host journal.
      needle = "${agenixFixture}/plaintext.txt"
      checks = [
          ("CP state.db", "cat /var/lib/nixfleet-cp/state.db 2>/dev/null"),
          ("CP audit.log", "cat /var/lib/nixfleet-cp/audit.log 2>/dev/null"),
          ("CP journal", "journalctl -u nixfleet-control-plane.service --no-pager"),
          ("CP /etc tree", "find /etc/nixfleet-cp -type f -exec cat {} +"),
      ]
      leaks = []
      for label, cmd in checks:
          rc, _ = host.execute(f"{cmd} | grep -aFq -f {needle}")
          if rc == 0:
              leaks.append(label)
      if leaks:
          raise Exception(
              f"plaintext leaked into CP-resident state: {leaks}"
          )

      print(
          "fleet-harness-secret-hygiene: CP disk + journal contain "
          "zero bytes of the agent-side plaintext (ARCHITECTURE.md §8)."
      )
    '';
  }
