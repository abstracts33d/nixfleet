# Control-plane teardown scenario. Validates
# ARCHITECTURE.md §8: destroying the CP's database and rebuilding
# from empty state restores full fleet visibility within one
# reconcile cycle.
#
# Sequence:
#   1. Boot host VM running cp-real + N agent microVMs running
#      agent-real.
#   2. Wait for both agents to log at least one successful checkin
#      ("checkin received" line in the CP journal). This proves
#      the steady-state.
#   3. Stop the CP service, `rm -rf /var/lib/nixfleet-cp/state.db*`
#      (matches the runbook's wipe step), restart the service.
#   4. Wait for both agents to log a NEW checkin (post-restart).
#      The agents are on a 10s poll cadence — recovery must
#      complete within 30s.
#   5. Assert each agent's post-restart checkin lands while the
#      verified-fleet snapshot also reprimes from the on-disk
#      signed artifact.
#
# What this proves:
#   - The CP can be restarted from empty SQLite state and resumes
#     accepting agent checkins immediately.
#   - The in-memory `host_checkins` projection repopulates on the
#     next agent checkin cycle (soft-state recovery; orphan-confirm
#     is dormant here because no rollouts are in flight).
#   - The verified-fleet snapshot reprimes from the build-time
#     artifact path on restart (no GitOps poll wired in this
#     scenario; the file-backed prime is the recovery source).
#
# What this does NOT yet prove (deferred):
#   - cert_revocations replay from a signed sidecar — needs the
#     harness to bake a signed revocations.json fixture.
#   - host_rollout_state recovery via the agent-attested
#     last_confirmed_at field — needs the agent to actually post a
#     confirm, which requires a closureHash matching
#     /run/current-system in the fixture (currently null per
#     fleet-resolved.json so dispatch returns NoDeclaration).
#   - pending_confirms orphan-recovery — needs an in-flight
#     dispatch at wipe time, same dependency.
#
# All three deferred properties are covered by per-table unit
# tests (see crates/nixfleet-control-plane/src/db.rs#tests). This
# scenario is the integration-level proof that the agent-side
# repopulation property holds end-to-end.
{
  lib,
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  agentPkg,
  # Optional signed revocations sidecar. When supplied, the CP runs
  # with --revocations-* flags pointed at a static HTTP server on the
  # host VM, and the post-wipe step asserts the sidecar replays into
  # `cert_revocations` within one poll cycle.
  revocationsFixture ? null,
  agentNames ? ["agent-01" "agent-02"],
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg revocationsFixture;
  };

  mkAgent = name:
    harnessLib.mkRealAgentNode {
      inherit testCerts signedFixture agentPkg;
      hostName = name;
      pollIntervalSecs = 10;
    };

  agents = lib.listToAttrs (map (n: {
      name = n;
      value = mkAgent n;
    })
    agentNames);
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-teardown";
    inherit cpHostModule agents;
    timeout = 900;
    testScript = let
      assertRevocationsReplayed = lib.optionalString (revocationsFixture != null) ''

        # Hard-state recovery: the signed revocations.json sidecar
        # must replay into `cert_revocations` after the wipe. Poll
        # interval is 60s on the CP side, so give it 90s of slack.
        print("step 4: waiting for revocations sidecar replay…")
        rev_deadline = time.monotonic() + 90
        rev_seen = False
        while time.monotonic() < rev_deadline:
            rc, _ = host.execute(
                "journalctl -u nixfleet-control-plane.service "
                f"--since='{post_wipe_cursor}' --no-pager "
                "| grep -E 'revocations poll: list verified.*entries=1'"
            )
            if rc == 0:
                rev_seen = True
                break
            time.sleep(3)
        if not rev_seen:
            raise Exception(
                "revocations sidecar did not replay within 90s after CP wipe"
            )
        print("step 4: revocations sidecar replayed (1 entry verified)")
      '';
    in ''
      import time

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_for_unit("microvms.target", timeout=300)
      for vm in ${builtins.toJSON agentNames}:
          host.wait_for_unit(f"microvm@{vm}.service", timeout=300)


      def wait_for_checkins_since(cursor: str, timeout_s: int) -> dict:
          """Block until each agent in `agentNames` has at least one
          'checkin received' line in the CP journal AFTER `cursor`
          (a 'YYYY-MM-DD HH:MM:SS' timestamp from `date`), or fail.
          Returns a dict[hostname → first-seen-at-monotonic-secs]
          for recovery-time measurement."""
          deadline = time.monotonic() + timeout_s
          pending = set(${builtins.toJSON agentNames})
          seen_at = {}
          while pending and time.monotonic() < deadline:
              for hostname in list(pending):
                  # tracing's default formatter renders fields as
                  # `hostname="agent-01"` (quoted) or
                  # `hostname=agent-01` (unquoted) depending on the
                  # subscriber config. Match both via a grep that
                  # just looks for the hostname token on a "checkin
                  # received" line.
                  rc, _ = host.execute(
                      f"journalctl -u nixfleet-control-plane.service "
                      f"--since='{cursor}' --no-pager "
                      f"| grep -E 'checkin received.*{hostname}'"
                  )
                  if rc == 0:
                      seen_at[hostname] = time.monotonic()
                      pending.discard(hostname)
              if pending:
                  time.sleep(2)
          if pending:
              raise Exception(
                  f"agents did not check in within {timeout_s}s after {cursor}: {pending}"
              )
          return seen_at


      # Establish baseline: each agent must check in at least once
      # against the freshly-booted CP. The cursor is captured at
      # host time AFTER `microvm@agent-XX.service` reaches active
      # (= qemu launched), so the budget covers the full guest-side
      # boot + cert mount + agent startup + first poll cycle. Lab
      # boot of two microvms typically lands the first checkins
      # within 90-120s; 180s is the safe upper bound.
      print("step 1: waiting for initial checkins…")
      pre_wipe_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      pre_wipe = wait_for_checkins_since(pre_wipe_cursor, timeout_s=180)
      print(f"step 1: baseline checkins observed: {pre_wipe}")

      # Wipe step: stop the CP, delete the SQLite database,
      # restart. Mirrors the operator runbook in DISASTER-RECOVERY.md.
      print("step 2: simulating CP destruction (stop + DB wipe + restart)…")
      host.succeed("systemctl stop nixfleet-control-plane.service")
      host.succeed("rm -rf /var/lib/nixfleet-cp/state.db /var/lib/nixfleet-cp/state.db-wal /var/lib/nixfleet-cp/state.db-shm")
      # Sleep 2s before cursor capture so the cursor's wall-clock
      # second is comfortably after every pre-wipe checkin's
      # journal timestamp. journalctl --since='YYYY-MM-DD HH:MM:SS'
      # rounds DOWN to the second, so a pre-wipe checkin at second
      # T+0.5 and a cursor captured at second T+0.1 share the same
      # `--since` second-bucket and would surface as a false-
      # positive "post-wipe" line. The 2s gap eliminates the race.
      host.succeed("sleep 2")
      post_wipe_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      host.succeed("systemctl start nixfleet-control-plane.service")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      # Recovery window: agents are on 10s poll, give them 30s
      # margin (3 poll cycles) to land a fresh checkin against the
      # post-restart CP. ARCHITECTURE.md §8's "one reconcile cycle"
      # with the harness's 10s poll = ~10-20s expected; 30s budget
      # is comfortable.
      print("step 3: waiting for post-wipe recovery checkins…")
      recovery_start = time.monotonic()
      post_wipe = wait_for_checkins_since(post_wipe_cursor, timeout_s=30)
      recovery_end = max(post_wipe.values())
      recovery_secs = recovery_end - recovery_start
      print(
          "step 3: post-wipe checkins observed in "
          f"{recovery_secs:.1f}s (budget 30s)"
      )

      # Surface the verified-fleet reprime in the journal so an
      # operator reading the test log sees the snapshot reload.
      host.succeed(
          "journalctl -u nixfleet-control-plane.service "
          f"--since='{post_wipe_cursor}' --no-pager "
          "| grep -E 'verified-fleet snapshot|primed verified-fleet'"
      )

      ${assertRevocationsReplayed}

      print(
          "fleet-harness-teardown: every agent re-checked-in within "
          "one reconcile cycle after CP DB wipe (ARCHITECTURE.md §8)."
      )
    '';
  }
