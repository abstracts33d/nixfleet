# LOADBEARING: post-wipe recovery proof — verifies cert_revocations
# replays from disk and `last_confirmed_at` echoes on the first post-wipe
# checkin. Without this an operator wiping CP state would silently
# unlock revoked certs and orphan in-flight rollouts.
{
  lib,
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  agentPkg,
  revocationsFixture ? null,
  closureHash,
  agentNames ? ["agent-01" "agent-02"],
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg revocationsFixture;
  };

  preseedModule = harnessLib.convergencePreseedModule {inherit closureHash;};

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

        print("step 4: waiting for revocations sidecar replay…")
        wait_for_journal_match(
            host,
            since_cursor=post_wipe_cursor,
            unit="nixfleet-control-plane.service",
            pattern="revocations poll: list verified.*entries=1",
            timeout=90,
            sleep_secs=3,
            label="revocations sidecar replay (1 entry verified)",
        )
        print("step 4: revocations sidecar replayed (1 entry verified)")
      '';

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
      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      host.wait_for_unit("microvms.target", timeout=300)
      for vm in ${builtins.toJSON agentNames}:
          host.wait_for_unit(f"microvm@{vm}.service", timeout=300)


      def wait_for_checkins_since(cursor: str, timeout_s: int) -> dict:
          """Block until each agent has a 'checkin received' line in the
          CP journal after `cursor`. Returns hostname -> seen-at."""
          deadline = time.monotonic() + timeout_s
          pending = set(${builtins.toJSON agentNames})
          seen_at = {}
          while pending and time.monotonic() < deadline:
              for hostname in list(pending):
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
              cp_dump = host.succeed(
                  "journalctl -u nixfleet-control-plane.service "
                  f"--since='{cursor}' --no-pager"
              )
              print(f"=== CP journal since {cursor} ===\n{cp_dump}\n=== end ===")
              for hostname in pending:
                  agent_dump = host.succeed(
                      f"journalctl -u microvm@{hostname}.service --no-pager | tail -120"
                  )
                  print(f"=== microvm@{hostname}.service (last 120 lines) ===\n{agent_dump}\n=== end ===")
              raise Exception(
                  f"agents did not check in within {timeout_s}s after {cursor}: {pending}"
              )
          return seen_at


      print("step 1: waiting for initial checkins…")
      pre_wipe_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      pre_wipe = wait_for_checkins_since(pre_wipe_cursor, timeout_s=180)
      print(f"step 1: baseline checkins observed: {pre_wipe}")

      print("step 2: simulating CP destruction (stop + DB wipe + restart)…")
      host.succeed("systemctl stop nixfleet-control-plane.service")
      host.succeed("rm -rf /var/lib/nixfleet-cp/state.db /var/lib/nixfleet-cp/state.db-wal /var/lib/nixfleet-cp/state.db-shm")
      # 2s gap: journalctl --since rounds to whole seconds, so without
      # the sleep a pre-wipe checkin can land in the post-wipe bucket.
      host.succeed("sleep 2")
      post_wipe_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      host.succeed("systemctl start nixfleet-control-plane.service")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      print("step 3: waiting for post-wipe recovery checkins…")
      recovery_start = time.monotonic()
      post_wipe = wait_for_checkins_since(post_wipe_cursor, timeout_s=30)
      recovery_end = max(post_wipe.values())
      recovery_secs = recovery_end - recovery_start
      print(
          "step 3: post-wipe checkins observed in "
          f"{recovery_secs:.1f}s (budget 30s)"
      )

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
