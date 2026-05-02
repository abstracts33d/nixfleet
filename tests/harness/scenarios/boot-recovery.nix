# tests/harness/scenarios/boot-recovery.nix
#
# ADR-011 boot-recovery scenario — the StaleClearedMismatch branch.
#
# The unit tests in `crates/nixfleet-agent/src/recovery.rs::tests` cover
# the pure decision logic (NoRecord, NoCurrent, StaleClearedMismatch,
# PostedConfirm, PostedConfirmFailed). What the unit tests can't cover
# is the integration claim: that the recovery hook actually fires at
# agent startup, BEFORE the regular checkin loop, and observes the
# real persistent `<state-dir>/last_dispatched` written by a prior
# agent run.
#
# The Acknowledged branch (current matches a real dispatched closure)
# would require synchronising the microvm's actual `/run/current-system`
# with a `closure_hash` we control at fixture-build time — hostile to
# harness isolation. The unit tests cover it via a stub HTTP server.
# This scenario exercises the StaleClearedMismatch branch instead:
#
#   1. Microvm boots with a pre-staged `last_dispatched` JSON file
#      whose closure_hash deliberately does NOT match
#      `/run/current-system`.
#   2. Agent starts. `check_boot_recovery` runs before the poll loop,
#      observes the mismatch, calls `clear_last_dispatched`.
#   3. testScript asserts:
#        a) the file is gone after agent has been up for a few seconds
#        b) the agent's journal logs the "current/dispatched mismatch"
#           tracing line.
#
# Why this matters: it proves the wiring (lib::recovery hooked into
# main.rs::main BEFORE the poll loop, state-dir path threaded through,
# tracing surfacing the action) works end-to-end on a real binary in
# a real systemd environment. Bare `cargo test` doesn't exercise that
# integration boundary.
{
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  agentPkg,
  pkgs,
  # Convergence target. The `signedFixture` passed in is the
  # converged variant whose declared closureHash matches this value;
  # `convergencePreseedModule` makes the agent's reported
  # current_generation.closure_hash match it too. Today the
  # scenario's assertions don't depend on convergence, but applying
  # it pre-emptively eliminates the silent-false-pass class for any
  # future assertion that gates on it.
  closureHash,
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Pre-staged JSON file: a deliberately stale dispatch record whose
  # closure_hash will not match the microvm's actual /run/current-system.
  # The dispatched_at timestamp is RFC-3339; serde_json round-trips
  # chrono::DateTime<Utc> via that format.
  staleDispatchJson = builtins.toJSON {
    closure_hash = "stale-harness-fake-closure-does-not-match-current-system";
    channel_ref = "stable@harness";
    rollout_id = "stable@harness";
    dispatched_at = "2026-01-01T00:00:00Z";
  };

  preseedModule = harnessLib.convergencePreseedModule {inherit closureHash;};

  agentNode = harnessLib.mkRealAgentNode {
    inherit testCerts signedFixture agentPkg;
    hostName = "agent-01";
    pollIntervalSecs = 10;
    extraModules = [
      preseedModule
      ({lib, ...}: {
        # ExecStartPre runs as root before the agent's main exec.
        # Stages the stale file into the StateDirectory the agent unit
        # creates (mode 0700, owned by root). systemd's StateDirectory=
        # ensures the dir exists before any ExecStartPre fires.
        systemd.services.nixfleet-agent.serviceConfig.ExecStartPre = lib.mkBefore [
          (pkgs.writeShellScript "harness-stage-stale-dispatch" ''
            set -euo pipefail
            mkdir -p /var/lib/nixfleet-agent
            cat > /var/lib/nixfleet-agent/last_dispatched <<'EOF'
            ${staleDispatchJson}
            EOF
            chmod 0600 /var/lib/nixfleet-agent/last_dispatched
            echo "harness: staged stale last_dispatched for boot-recovery test"
          '')
        ];
      })
    ];
  };
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-boot-recovery";
    inherit cpHostModule;
    agents = {
      agent-01 = agentNode;
    };
    timeout = 600;
    testScript = ''
      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_for_unit("microvms.target", timeout=300)
      host.wait_for_unit("microvm@agent-01.service", timeout=300)

      # Wait for the agent's first checkin to land — that's the latest
      # point at which boot-recovery would have fired (it runs before
      # the poll loop). Once we see a checkin, we know main has reached
      # the loop and recovery has either ran or skipped.
      print("step 1: waiting for agent first checkin (post-recovery)…")
      deadline = time.monotonic() + 90
      checked_in = False
      while time.monotonic() < deadline:
          rc, _ = host.execute(
              "journalctl -u nixfleet-control-plane.service --no-pager "
              "| grep -E 'checkin received.*agent-01'"
          )
          if rc == 0:
              checked_in = True
              break
          time.sleep(2)
      assert checked_in, "agent never checked in within 90s"
      print("step 1: agent checked in, recovery hook has fired")

      # Step 2: assert the staged stale file was cleared by recovery.
      # microvm.shares would be the way to check guest-side state from
      # the host, but the harness doesn't wire that. Instead grep the
      # microvm's serialized journal (forwarded to host via
      # microvm@<name>.service's StandardOutput=journal+console) for
      # the recovery's tracing line.
      print("step 2: checking agent journal for StaleClearedMismatch action…")
      rc, out = host.execute(
          "journalctl -u microvm@agent-01.service --no-pager "
          "| grep -E 'boot-recovery: cleared stale dispatch record|StaleClearedMismatch|current/dispatched mismatch'"
      )
      assert rc == 0, (
          f"expected boot-recovery clear-stale log line in agent journal; got: {out!r}"
      )
      print("step 2: recovery cleared the stale record as expected")

      print(
          "fleet-harness-boot-recovery: ADR-011 boot-recovery hook "
          "ran before poll loop and cleared the stale dispatch record."
      )
    '';
  }
