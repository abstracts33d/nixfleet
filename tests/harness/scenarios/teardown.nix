# Control-plane teardown scenario. Validates
# ARCHITECTURE.md §8: destroying the CP's database and rebuilding
# from empty state restores full fleet visibility within one
# reconcile cycle.
#
# Sequence:
#   1. Boot host VM running cp-real + N agent microVMs running
#      agent-real. Each agent is pre-seeded with last_confirmed_at
#      and an overridden /run/current-system so its reported
#      closure_hash matches the fleet's declared expectation.
#   2. Wait for both agents to log at least one successful checkin
#      ("checkin received" line in the CP journal). Steady-state.
#   3. Stop the CP service, `rm -rf /var/lib/nixfleet-cp/state.db*`
#      (matches the runbook's wipe step), restart the service.
#   4. Wait for the post-restart CP to:
#      - accept fresh checkins from each agent (soft-state replay),
#      - replay the signed revocations.json sidecar into
#        cert_revocations within one poll cycle,
#      - apply the agent-attested last_confirmed_at to repopulate
#        host_rollout_state.last_healthy_since (soak-state recovery).
#
# What this proves:
#   - CP restart from empty SQLite resumes accepting checkins
#     within one reconcile cycle (soft-state).
#   - verified_fleet snapshot reprimes from the build-time signed
#     artifact path.
#   - cert_revocations rebuilds from the signed sidecar (hard-state,
#     security-material).
#   - host_rollout_state.last_healthy_since rebuilds from the
#     agent-attested last_confirmed_at field on convergence.
{
  lib,
  pkgs,
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
  # Convergence target for the soak-state recovery proof. Must match
  # the closureHash injected into `signedFixture`'s fleet.resolved
  # for both agents — the preseedModule below makes the agent's
  # /run/current-system resolve to a path with this basename so
  # current_generation.closure_hash matches fleet.hosts[*].closureHash.
  teardownClosureHash,
  agentNames ? ["agent-01" "agent-02"],
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg revocationsFixture;
  };

  # Pre-seeds last_confirmed_at and overrides /run/current-system so
  # each agent reports the convergence closure_hash + echoes a
  # synthetic attestation timestamp on every checkin. Both pieces
  # are required for the CP's recover_soak_state_from_attestation
  # path to apply: the closure match unlocks the recovery, the
  # attestation provides the timestamp to clamp into
  # host_rollout_state.last_healthy_since.
  attestedAt = "2026-04-01T00:00:00Z";
  preseedModule = {pkgs, ...}: {
    systemd.services.harness-agent-preseed = {
      description = "Pre-seed agent state-dir + override /run/current-system for convergence";
      wantedBy = ["multi-user.target"];
      before = ["nixfleet-agent.service"];
      after = ["local-fs.target"];
      # `requiredBy` makes the agent unit fail loudly if preseed
      # fails. Without it, read_last_confirmed silently returns
      # Ok(None) on a missing file and the recovery test would
      # false-pass.
      requiredBy = ["nixfleet-agent.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        set -euo pipefail

        # Override /run/current-system. The symlink target doesn't
        # need to exist — the agent reads the symlink string via
        # fs::read_link and reports its basename.
        ${pkgs.coreutils}/bin/ln -sfn \
          /tmp/${teardownClosureHash} /run/current-system

        # Seed last_confirmed_at in the agent's state dir. Format
        # per crates/nixfleet-agent/src/checkin_state.rs:
        # <closure_hash>\n<rfc3339>\n. Two-line plaintext.
        ${pkgs.coreutils}/bin/mkdir -p /var/lib/nixfleet-agent
        ${pkgs.coreutils}/bin/chmod 0700 /var/lib/nixfleet-agent
        ${pkgs.coreutils}/bin/printf '%s\n%s\n' \
          '${teardownClosureHash}' '${attestedAt}' \
          > /var/lib/nixfleet-agent/last_confirmed_at
        ${pkgs.coreutils}/bin/chmod 0600 \
          /var/lib/nixfleet-agent/last_confirmed_at
      '';
    };
  };

  mkAgent = name:
    harnessLib.mkRealAgentNode {
      inherit testCerts signedFixture agentPkg;
      hostName = name;
      pollIntervalSecs = 10;
      extraModules = [preseedModule];
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

      # Soft-state attestation recovery: each agent's pre-seeded
      # last_confirmed_at must echo on its first post-wipe checkin
      # and trigger recover_soak_state_from_attestation, which
      # stamps host_rollout_state.last_healthy_since from the
      # attested timestamp. The CP emits the load-bearing log line
      # from checkin_pipeline::recover_soak_state_from_attestation.
      assertSoakStateRecovered = ''

        print("step 5: waiting for soak-state attestation recovery…")
        soak_deadline = time.monotonic() + 60
        recovered: set[str] = set()
        agents_set: set[str] = set(${builtins.toJSON agentNames})
        while recovered != agents_set and time.monotonic() < soak_deadline:
            for hostname in list(agents_set - recovered):
                rc, _ = host.execute(
                    "journalctl -u nixfleet-control-plane.service "
                    f"--since='{post_wipe_cursor}' --no-pager "
                    "| grep -E "
                    f"'soak-state recovery: stamped last_healthy_since.*{hostname}'"
                )
                if rc == 0:
                    recovered.add(hostname)
            if recovered != agents_set:
                time.sleep(3)
        missing = agents_set - recovered
        if missing:
            # Diagnostic dump: surface the post-wipe CP journal AND
            # each missing agent's console-forwarded journal so the
            # failure mode is debuggable from build logs alone.
            cp_dump = host.succeed(
                "journalctl -u nixfleet-control-plane.service "
                f"--since='{post_wipe_cursor}' --no-pager"
            )
            print("=== post-wipe CP journal ===")
            print(cp_dump)
            print("=== end CP journal ===")
            for missing_host in sorted(missing):
                vm_dump = host.succeed(
                    f"journalctl -u microvm@{missing_host}.service --no-pager"
                )
                print(f"=== {missing_host} microvm journal ===")
                print(vm_dump)
                print(f"=== end {missing_host} microvm journal ===")
            raise Exception(
                f"soak-state recovery did not stamp last_healthy_since "
                f"for {missing} within 60s after CP wipe"
            )
        print("step 5: soak-state recovery stamped last_healthy_since "
              f"for {len(recovered)} agents")
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
      ${assertSoakStateRecovered}

      print(
          "fleet-harness-teardown: every agent re-checked-in within "
          "one reconcile cycle after CP DB wipe; revocations sidecar "
          "replayed and soak-state attestation recovery stamped "
          "host_rollout_state (ARCHITECTURE.md §8)."
      )
    '';
  }
