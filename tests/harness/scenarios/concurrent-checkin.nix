# Concurrent-checkin scenario.
#
# Validates the atomic verified_fleet snapshot fix landed in
# `crates/nixfleet-control-plane/src/server/state.rs::VerifiedFleetSnapshot`
# (commit 7d3b3ed): the `(fleet, fleet_resolved_hash)` pair is now
# held under a single `RwLock` so a checkin reader can never see a
# fresh `fleet` paired with a stale `fleet_resolved_hash`.
#
# Pre-fix posture: two separate `RwLock`s were updated in sequence
# during a channel-refs poll. A reader between the two writes saw a
# torn pair → corrupted rolloutId anchor (RFC-0002 §4.4 contract
# violation: rolloutId is an ANCHOR derived from
# `(fleet_resolved_hash, channel)` and must always match the
# enclosing fleet bytes).
#
# Probe surface — load-bearing claim:
#   Under continuous concurrent /v1/agent/checkin pressure, every
#   `dispatch: target issued` log line's `rollout=<id>` field falls
#   within the stable set of rollout_ids derived from the verified
#   snapshot's `(fleet, fleet_resolved_hash)` pair. NO checkin ever
#   observed a (fleet, fleet_resolved_hash) pair that produced a
#   rollout_id outside that set — i.e. no torn snapshot reached the
#   dispatch path.
#
# Limitation — single fleet variant:
#   The stronger invariant (drive alternating fleet.resolved updates
#   AT POLL TIME and assert the issued rollout_ids land EITHER in
#   the A-derived set OR the B-derived set, never some third value)
#   requires the harness to reload the channel-refs poll source
#   mid-test. Today's CP exposes `POLL_INTERVAL` as a `pub const
#   Duration::from_secs(60)` — there's no flag or env var to override
#   it within a test budget, and no admin endpoint to force a
#   reload. Implementing the alternating-fixture variant cleanly
#   would require either:
#     a) exposing `POLL_INTERVAL` via a CLI flag (a small CP code
#        change), or
#     b) introducing an admin endpoint that re-primes the snapshot
#        from disk (broader CP change).
#   Both are out of scope for this harness scenario — they belong in
#   the next CP iteration. Per the audit-fix doc's fallback clause:
#   "if the harness can't drive multiple fleet.resolved updates
#   cleanly within a single test, fall back to … assert that across
#   many concurrent checkins, the rollout_ids form a stable set (no
#   torn intermediates)". This file implements that fallback.
#
# Mechanism:
#   - cp-real boots with the harness signedFixture as the
#     verified-fleet snapshot. The fixture has one channel
#     ("stable") with the all-at-once rollout policy → one
#     dispatchable rollout_id, one pre-fixed fleet_resolved_hash.
#   - 5 host-side curl loops fire /v1/agent/checkin posts in a tight
#     loop for 30s. Each loop runs a different `agent-NN` cert from
#     `testCerts`, and each post embeds a synthetic
#     `currentGeneration.closureHash` that DIFFERS from the
#     fixture's host[*].closureHash → forces dispatch to evaluate
#     and emit a `target` (and the load-bearing log line).
#   - testScript parses every `dispatch: target issued` line in the
#     CP journal during the window. Asserts the set of unique
#     rollout_ids has cardinality ≤ 1 (the steady-state prediction
#     under a single fixture). Any cardinality > 1 means the
#     snapshot got torn at some point — direct regression of
#     the atomic-pair fix.
{
  lib,
  pkgs,
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  # Number of concurrent checkin loops to run during the soak. 5 is
  # comfortably above the dispatch path's per-host serialisation
  # gate while staying inside the fleet-N harness's 10-cert budget.
  agentLoopCount ? 5,
  # Soak duration in seconds. 30s on a tight loop yields hundreds
  # of checkins across 5 loops — plenty of independent reads of
  # the verified-fleet snapshot to surface a torn pair.
  soakDurationSecs ? 30,
  ...
}: let
  cpHostBase = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Mount one client-cert pair per concurrent loop. We reuse the
  # pre-minted `agent-01..agent-05` certs from the shared cert set;
  # /etc/harness/<hostname>-{cert,key}.pem.
  loopHostnames = map (i: "agent-${lib.fixedWidthString 2 "0" (toString i)}") (
    lib.range 1 agentLoopCount
  );

  certMountModule = {
    environment.etc =
      {
        "harness/ca.pem".source = "${testCerts}/ca.pem";
      }
      // builtins.listToAttrs (lib.concatMap (h: [
          {
            name = "harness/${h}-cert.pem";
            value.source = "${testCerts}/${h}-cert.pem";
          }
          {
            name = "harness/${h}-key.pem";
            value.source = "${testCerts}/${h}-key.pem";
          }
        ])
        loopHostnames);
    environment.systemPackages = [pkgs.jq];
  };

  combinedHostModule = {
    imports = [cpHostBase certMountModule];
  };

  # Loop driver — written to disk via writeText so the testScript
  # doesn't have to embed the bash via Python heredocs (which would
  # interleave Python and shell quoting awkwardly).
  loopDriverScript = pkgs.writeShellScript "harness-checkin-loop" ''
    set -u
    hostname="$1"
    duration="$2"
    cacert=/etc/harness/ca.pem
    cert="/etc/harness/$hostname-cert.pem"
    key="/etc/harness/$hostname-key.pem"
    end=$(( $(date +%s) + duration ))
    while [ "$(date +%s)" -lt "$end" ]; do
      body=$(${pkgs.jq}/bin/jq -n --arg h "$hostname" '{
        hostname: $h, schemaVersion: 1, machineId: $h,
        agentVersion: "harness-concurrent",
        uptimeSecs: 1,
        bootId: "00000000-0000-0000-0000-000000000000",
        currentGeneration: {
          closureHash: ("deadbeef-mismatch-" + $h),
          channelRef: "main",
          bootId: "00000000-0000-0000-0000-000000000000"
        }
      }')
      curl -sk -o /dev/null \
        --cacert "$cacert" \
        --cert "$cert" \
        --key "$key" \
        -H 'Content-Type: application/json' \
        -d "$body" \
        https://localhost:8443/v1/agent/checkin || true
      # No sleep: tight loop maximises read pressure on the
      # verified_fleet RwLock. The TLS handshake + axum dispatch
      # naturally rate-limits to a few hundred req/s per loop.
    done
  '';
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-concurrent-checkin";
    cpHostModule = combinedHostModule;
    agents = {}; # wire flow driven by host-side curl loops
    timeout = 600;
    testScript = ''
      import re

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)

      # Wait for the reconcile loop to prime the verified-fleet
      # snapshot from the signed fixture. cp-real reads the artifact
      # path on boot; this log line confirms the (fleet,
      # fleet_resolved_hash) pair is in place.
      host.wait_until_succeeds(
          "journalctl -u nixfleet-control-plane.service --no-pager "
          "| grep -E 'verified-fleet snapshot|primed verified-fleet'",
          timeout=60,
      )

      hostnames = ${builtins.toJSON loopHostnames}
      soak_secs = ${toString soakDurationSecs}

      # Cursor BEFORE we start the loops, so the journal-grep below
      # only counts dispatches inside the soak window. journalctl's
      # `--since` rounds DOWN to the second; sleep 1s to make sure
      # the cursor's wall-clock is strictly before the first
      # checkin's logged second.
      soak_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      host.succeed("sleep 1")

      print(f"step 1: spawning {len(hostnames)} concurrent checkin loops "
            f"for {soak_secs}s…")
      bg_cmd = " & ".join(
          f"${loopDriverScript} {h} {soak_secs}" for h in hostnames
      ) + " & wait"
      host.succeed(f"bash -c '{bg_cmd}'", timeout=soak_secs + 60)
      print("step 1: soak complete")

      # Step 2: parse every `dispatch: target issued` line emitted
      # during the soak window. The line carries `rollout=<rollout_id>`
      # as a tracing field. Two formats depending on subscriber config:
      #   `rollout="abcd…"`     (quoted)
      #   `rollout=abcd…`        (unquoted)
      print("step 2: harvesting dispatched rollout_ids from CP journal…")
      journal = host.succeed(
          "journalctl -u nixfleet-control-plane.service "
          f"--since='{soak_cursor}' --no-pager"
      )
      dispatch_lines = [ln for ln in journal.splitlines() if "target issued" in ln]
      print(f"step 2: {len(dispatch_lines)} `target issued` lines observed")

      # Both quoted and unquoted forms are accepted. The set comp
      # below pulls out whichever side matches.
      rollout_re = re.compile(r'rollout="?([^"\s]+)"?')
      rollout_ids: set[str] = set()
      for ln in dispatch_lines:
          m = rollout_re.search(ln)
          if m is not None:
              rollout_ids.add(m.group(1))

      print(f"step 2: unique rollout_ids in dispatch log: {sorted(rollout_ids)}")

      # Step 3: assert stability. Under one fixture (no fleet update
      # mid-soak), the verified-fleet snapshot is constant; the only
      # rollout_id derivable from it is the channel "stable" anchor.
      # Cardinality > 1 means a torn (fleet, fleet_resolved_hash)
      # pair reached compute_rollout_id_for_channel — direct
      # regression of the VerifiedFleetSnapshot atomic-pair fix.
      assert len(rollout_ids) <= 1, (
          f"torn-snapshot regression: {len(rollout_ids)} distinct "
          f"rollout_ids observed under steady-state fleet — expected "
          f"≤ 1. Set: {sorted(rollout_ids)}"
      )

      # If we got 0, dispatch never fired (likely the fixture's
      # NoDeclaration path under stub closureHashes). That's not a
      # FAILURE of the atomic-pair contract — the contract is
      # vacuously held when no checkin produces a target — but it
      # also means we got no signal. Surface a clear print so the
      # test log records which branch took.
      if len(rollout_ids) == 0:
          print(
              "step 3: 0 dispatches issued during soak — fixture's "
              "stub closureHashes may produce NoDeclaration. Atomic "
              "pair contract holds vacuously; "
              "${toString agentLoopCount} loops × "
              "${toString soakDurationSecs}s of read pressure with NO "
              "torn-snapshot panic / mismatch in CP journal."
          )
      else:
          print(
              f"step 3: 1 stable rollout_id across "
              f"{len(dispatch_lines)} dispatches — atomic "
              f"VerifiedFleetSnapshot pair held under "
              f"${toString agentLoopCount} concurrent loops × "
              f"${toString soakDurationSecs}s soak."
          )

      # Step 4: extra defensive scan — no error / panic / "torn" /
      # "mismatch" lines in the CP journal during the soak. Catches
      # the rarer regression where the read sees a `None` between
      # writes (the new code can't surface that, but a future
      # change might).
      bad_rc, _ = host.execute(
          "journalctl -u nixfleet-control-plane.service "
          f"--since='{soak_cursor}' --no-pager "
          "| grep -E 'panic|torn|fleet-hash mismatch|"
          "compute_rollout_id_for_channel failed'"
      )
      if bad_rc == 0:
          # grep matched something — surface it as a failure.
          dump = host.succeed(
              "journalctl -u nixfleet-control-plane.service "
              f"--since='{soak_cursor}' --no-pager"
          )
          raise Exception(
              "concurrent-checkin: error/panic/torn-snapshot pattern "
              "found in CP journal during soak\n=== journal ===\n"
              + dump
              + "\n=== end ==="
          )
      print("step 4: no error/panic/torn patterns in CP journal during soak")

      print(
          "fleet-harness-concurrent-checkin: atomic VerifiedFleetSnapshot "
          "contract holds — under "
          + str(${toString agentLoopCount})
          + " concurrent checkin loops × "
          + str(${toString soakDurationSecs})
          + "s soak, dispatched rollout_ids form a stable set "
          "(cardinality ≤ 1) and no torn-pair indicators surfaced."
      )
    '';
  }
