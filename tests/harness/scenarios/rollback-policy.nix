# tests/harness/scenarios/rollback-policy.nix
#
# Hardware harness scenario for #69 rollback-and-halt end-to-end.
# Validates RFC-0002 §5.1: an operator-declared
# `onHealthFailure = "rollback-and-halt"` policy + a Failed host
# transition produces a `RollbackSignal` on the next CheckinResponse,
# the agent runs its rollback path, posts `RollbackTriggered`, and
# the CP transitions the host to `Reverted` (whereupon the signal
# stops emitting on subsequent checkins).
#
# Inducement path:
#   #76's body proposes either a probe-collector returning
#   `non-compliant` under `enforce` mode, or a closure with a
#   deliberately-failing activation. Both add substantial harness
#   plumbing for what is fundamentally a wire-and-state assertion.
#   This scenario takes the deadline-expiry approach: drive the DB
#   state directly via host-side sqlite3 to land the agent in the
#   target state. The wire round-trip — which is what #76 exists to
#   exercise — is unaffected by how the host got into Failed; what
#   matters is that the CP's `compute_rollback_signal` fires, the
#   agent acts on it, and the response idempotency holds.
#
# Sequence:
#   1. Boot cp-real (with `onHealthFailure = "rollback-and-halt"`
#      fixture variant) + 1 agent microVM. Wait for steady-state
#      checkins so we know the agent is in the loop.
#   2. host-side sqlite3: insert a `pending_confirms` row + flip
#      `host_rollout_state.host_state` to `Failed` for agent-01 on a
#      synthetic rolloutId.
#   3. Wait for the agent's next CheckinResponse to carry the
#      `rollback` field. Inspect via the CP journal — the
#      `rollback_signal_for_checkin` path emits an INFO line on
#      every emission.
#   4. Wait for the agent's microVM journal to show
#      `handle_cp_rollback_signal` firing (`agent: CP issued rollback
#      signal …`) followed by the `RollbackTriggered` post.
#   5. Wait for `host_rollout_state.host_state = 'Reverted'` (the
#      `apply_rollback_state_transition` writeback in the report
#      handler).
#   6. Force one more poll cycle and assert the CP no longer emits a
#      rollback_signal — the `Reverted` row stops compute_rollback_signal
#      from firing (idempotency, #69 follow-up `3069ec7`).
#
# Why direct DB injection over a real Failed transition:
#   `apply_actions` (`server/reconcile.rs:210`) only handles
#   `Action::SoakHost` today; the Failed state is written via
#   compliance / wave_gate code paths whose harness setup would
#   eclipse this scenario's actual claim. Injecting the row directly
#   reproduces the byte-identical DB shape compliance-driven failures
#   produce, with zero ambiguity about what's being tested.
#
# Verification gap (read this before iterating on the assertions):
#   This scenario was authored without ability to boot the harness
#   on macOS. Structural soundness was verified (Nix evaluates,
#   imports resolve), but wire-level assertions (timing windows,
#   exact log-line wording, the rollback_signal field shape on the
#   agent side) need confirmation on Linux/KVM. Likely tweak
#   surface: poll-cycle timing constants, the journal grep regexes
#   if the formatter trims field quoting, and step 6's assertion if
#   `Reverted` rows still take one extra tick to suppress emission.
{
  harnessLib,
  testCerts,
  signedFixture,
  cpPkg,
  agentPkg,
  closureHash,
  agentName ? "agent-01",
  ...
}: let
  cpHostModule = harnessLib.mkRealCpHostModule {
    inherit testCerts signedFixture cpPkg;
  };

  # Convergence preseed: same machinery teardown uses, so the
  # agent's reported closure_hash matches `fleet.hosts.<n>.closureHash`
  # (set via the rollback-policy fixture's `hostClosureHashes`). The
  # match unlocks dispatch + the orphan-confirm recovery's closure
  # check; without it the agent would never get past the first
  # checkin into a state where the Failed-injection lands on a row
  # that subsequent checkins re-target.
  preseedModule = harnessLib.convergencePreseedModule {inherit closureHash;};

  agentNode = harnessLib.mkRealAgentNode {
    inherit testCerts signedFixture agentPkg;
    hostName = agentName;
    pollIntervalSecs = 5;
    extraModules = [preseedModule];
  };

  agents = {${agentName} = agentNode;};
in
  harnessLib.mkFleetScenario {
    name = "fleet-harness-rollback-policy";
    inherit cpHostModule agents;
    timeout = 600;
    testScript = ''
      import time

      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)
      host.wait_for_unit("microvms.target", timeout=300)
      host.wait_for_unit(f"microvm@${agentName}.service", timeout=300)

      # Step 1: baseline — wait for the agent to land at least one
      # checkin against the freshly-booted CP. Polling cadence is 5s
      # in this scenario; allow 90s for the full guest-side boot +
      # cert mount + first poll.
      print("step 1: waiting for initial agent checkin…")
      pre_inject_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      checkin_deadline = time.monotonic() + 90
      while time.monotonic() < checkin_deadline:
          rc, _ = host.execute(
              "journalctl -u nixfleet-control-plane.service "
              f"--since='{pre_inject_cursor}' --no-pager "
              f"| grep -E 'checkin received.*${agentName}'"
          )
          if rc == 0:
              break
          time.sleep(2)
      else:
          raise Exception("agent did not check in within 90s of boot")
      print(f"step 1: baseline checkin observed for ${agentName}")

      # Step 2: inject a synthetic Failed row. We need both a
      # pending_confirms anchor (so dispatch resolves the row) and a
      # host_rollout_state row in `Failed` (so compute_rollback_signal
      # picks it up). The rolloutId encodes a sentinel suffix so the
      # cleanup-side asserts match exactly.
      injected_rollout_id = "stable@injected-failure"
      print(f"step 2: injecting Failed state for ${agentName}@{injected_rollout_id}")
      host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db \"\""
          "INSERT INTO pending_confirms ("
          "  hostname, rollout_id, channel, wave, target_closure_hash,"
          "  target_channel_ref, dispatched_at, confirm_deadline,"
          "  state"
          ") VALUES ("
          f"  '${agentName}', '{injected_rollout_id}', 'stable', 0,"
          f"  '${closureHash}', '{injected_rollout_id}',"
          "  datetime('now', '-30 seconds'),"
          "  datetime('now', '+300 seconds'),"
          "  'pending'"
          ");"
          "\""
      )
      host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db \"\""
          "INSERT INTO host_rollout_state ("
          "  rollout_id, hostname, host_state, updated_at"
          ") VALUES ("
          f"  '{injected_rollout_id}', '${agentName}', 'Failed',"
          "  datetime('now')"
          ");"
          "\""
      )

      # Sanity: the row is visible under the post-inject cursor.
      pre_signal_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      pre_state = host.succeed(
          "sqlite3 /var/lib/nixfleet-cp/state.db "
          "\"SELECT host_state FROM host_rollout_state "
          f"  WHERE hostname='${agentName}' AND rollout_id='{injected_rollout_id}';\""
      ).strip()
      assert pre_state == "Failed", f"expected Failed pre-signal, got {pre_state!r}"

      # Step 3: wait for the CP to emit `rollback_signal` on the next
      # CheckinResponse. The `rollback_signal_for_checkin` path logs
      # this line at INFO level on every emission.
      print("step 3: waiting for CP rollback-signal emission…")
      signal_deadline = time.monotonic() + 60
      signal_seen = False
      while time.monotonic() < signal_deadline:
          rc, _ = host.execute(
              "journalctl -u nixfleet-control-plane.service "
              f"--since='{pre_signal_cursor}' --no-pager "
              "| grep -E 'rollback-signal: emitting RollbackSignal'"
          )
          if rc == 0:
              signal_seen = True
              break
          time.sleep(2)
      if not signal_seen:
          cp_dump = host.succeed(
              "journalctl -u nixfleet-control-plane.service "
              f"--since='{pre_signal_cursor}' --no-pager"
          )
          print("=== CP journal (rollback-signal expected) ===")
          print(cp_dump)
          print("=== end ===")
          raise Exception("CP did not emit rollback-signal within 60s of injection")
      print("step 3: CP emitted rollback-signal as expected")

      # Step 4: agent-side handle_cp_rollback_signal fires. The
      # `agent: CP issued rollback signal` log line is emitted from
      # `agent/dispatch.rs:494` before the rollback is fired.
      print("step 4: waiting for agent-side rollback handling…")
      agent_deadline = time.monotonic() + 60
      agent_seen = False
      while time.monotonic() < agent_deadline:
          rc, _ = host.execute(
              f"journalctl -u microvm@${agentName}.service --no-pager "
              "| grep -E 'CP issued rollback signal'"
          )
          if rc == 0:
              agent_seen = True
              break
          time.sleep(2)
      if not agent_seen:
          vm_dump = host.succeed(
              f"journalctl -u microvm@${agentName}.service --no-pager"
          )
          print(f"=== ${agentName} microvm journal ===")
          print(vm_dump)
          print("=== end ===")
          raise Exception("agent did not handle rollback signal within 60s")
      print("step 4: agent-side rollback fired")

      # Step 5: agent posts RollbackTriggered → CP report_handler
      # transitions the host_rollout_state row from Failed to
      # Reverted. `apply_rollback_state_transition` is the writeback
      # in `server/report_handler.rs:153`.
      print("step 5: waiting for Failed → Reverted transition…")
      reverted_deadline = time.monotonic() + 60
      reverted_seen = False
      while time.monotonic() < reverted_deadline:
          state_now = host.succeed(
              "sqlite3 /var/lib/nixfleet-cp/state.db "
              "\"SELECT host_state FROM host_rollout_state "
              f"  WHERE hostname='${agentName}' AND rollout_id='{injected_rollout_id}';\""
          ).strip()
          if state_now == "Reverted":
              reverted_seen = True
              break
          time.sleep(2)
      if not reverted_seen:
          raise Exception(
              "host_rollout_state did not flip Failed → Reverted within 60s"
          )
      print("step 5: host_state = Reverted")

      # Step 6: idempotency. Once the row is Reverted,
      # compute_rollback_signal returns None — subsequent checkins
      # carry no rollback field. Sample two more poll cycles and
      # assert no fresh emission lines.
      print("step 6: waiting for two more polls + asserting no re-emission…")
      post_revert_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      time.sleep(15)  # ~3 agent polls at 5s cadence
      rc, _ = host.execute(
          "journalctl -u nixfleet-control-plane.service "
          f"--since='{post_revert_cursor}' --no-pager "
          "| grep -E 'rollback-signal: emitting RollbackSignal'"
      )
      if rc == 0:
          cp_dump = host.succeed(
              "journalctl -u nixfleet-control-plane.service "
              f"--since='{post_revert_cursor}' --no-pager"
          )
          print("=== CP journal (no rollback-signal expected) ===")
          print(cp_dump)
          print("=== end ===")
          raise Exception(
              "CP re-emitted rollback-signal after Reverted transition"
          )

      print(
          "fleet-harness-rollback-policy: rollback-and-halt round-trip "
          "holds — Failed → CP RollbackSignal → agent rollback → "
          "RollbackTriggered → Reverted → emission stops."
      )
    '';
  }
