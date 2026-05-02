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
#   2. host-side sqlite3: insert a `host_dispatch_state` operational
#      row + matching `dispatch_history` audit row + flip
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
#   6. Assert the host_dispatch_state operational row is parked at
#      `state='rolled-back'` and the dispatch_history audit row's
#      `terminal_state` is stamped `'rolled-back'`. The operational
#      row stays on disk until the next dispatch UPSERTs it;
#      `active_rollouts_snapshot` filters terminal states out, which
#      is what stops idempotent re-emission.
#   7. Force one more poll cycle and assert the CP no longer emits a
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

  # cp-real doesn't include sqlite3 in systemPackages; the testScript
  # needs the CLI on the host VM to drive the Failed-state inducement
  # (deadline-expiry has the same gap and fails silently — its
  # sqlite3 calls would 127 too if anyone ran it). Scenario-local
  # override avoids perturbing other scenarios.
  sqliteHostModule = {pkgs, ...}: {
    environment.systemPackages = [pkgs.sqlite];
  };

  combinedHostModule = {
    imports = [cpHostModule sqliteHostModule];
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
    cpHostModule = combinedHostModule;
    inherit agents;
    timeout = 600;
    testScript = ''
      start_all()

      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("nixfleet-control-plane.service")
      host.wait_for_open_port(8443)
      host.wait_for_unit("microvms.target", timeout=300)
      host.wait_for_unit("microvm@${agentName}.service", timeout=300)

      # Step 1: baseline — wait for the agent to land at least one
      # checkin against the freshly-booted CP. Polling cadence is 5s
      # in this scenario; allow 90s for the full guest-side boot +
      # cert mount + first poll.
      print("step 1: waiting for initial agent checkin…")
      pre_inject_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      wait_for_journal_match(
          host,
          since_cursor=pre_inject_cursor,
          unit="nixfleet-control-plane.service",
          pattern="checkin received.*${agentName}",
          timeout=90,
          label="initial agent checkin",
      )
      print("step 1: baseline checkin observed for ${agentName}")

      # Step 2: inject a synthetic Failed row. Three writes:
      # `host_dispatch_state` operational anchor (so dispatch
      # resolves the row), `dispatch_history` audit (so the terminal
      # stamp lands somewhere on report), and `host_rollout_state`
      # in `Failed` (so compute_rollback_signal picks it up). The
      # rolloutId encodes a sentinel suffix so the post-rollback
      # asserts match exactly.
      injected_rollout_id = "stable@injected-failure"
      print(f"step 2: injecting Failed state for ${agentName}@{injected_rollout_id}")
      # `host_dispatch_state.hostname` is PRIMARY KEY (one row per
      # host). The agent's first checkin already triggered the
      # orphan-confirm recovery path which UPSERT'd a row for
      # ${agentName}; a plain INSERT here would trip the UNIQUE
      # constraint. INSERT OR REPLACE ensures the injected `pending`
      # row wins, which is the state shape `compute_rollback_signal`
      # needs to fire.
      host.succeed(f"""sqlite3 /var/lib/nixfleet-cp/state.db <<'SQL'
      INSERT OR REPLACE INTO host_dispatch_state (
        hostname, rollout_id, channel, wave, target_closure_hash,
        target_channel_ref, state, dispatched_at, confirm_deadline
      ) VALUES (
        '${agentName}', '{injected_rollout_id}', 'stable', 0,
        '${closureHash}', '{injected_rollout_id}',
        'pending',
        datetime('now', '-30 seconds'),
        datetime('now', '+300 seconds')
      );
      SQL""")
      host.succeed(f"""sqlite3 /var/lib/nixfleet-cp/state.db <<'SQL'
      INSERT INTO dispatch_history (
        hostname, rollout_id, channel, wave, target_closure_hash,
        target_channel_ref, dispatched_at
      ) VALUES (
        '${agentName}', '{injected_rollout_id}', 'stable', 0,
        '${closureHash}', '{injected_rollout_id}',
        datetime('now', '-30 seconds')
      );
      SQL""")
      host.succeed(f"""sqlite3 /var/lib/nixfleet-cp/state.db <<'SQL'
      INSERT INTO host_rollout_state (
        rollout_id, hostname, host_state, updated_at
      ) VALUES (
        '{injected_rollout_id}', '${agentName}', 'Failed',
        datetime('now')
      );
      SQL""")

      # Sanity: the row is visible under the post-inject cursor.
      pre_signal_cursor = host.succeed("date '+%Y-%m-%d %H:%M:%S'").strip()
      pre_state = host.succeed(f"""sqlite3 /var/lib/nixfleet-cp/state.db <<'SQL'
      SELECT host_state FROM host_rollout_state
      WHERE hostname='${agentName}' AND rollout_id='{injected_rollout_id}';
      SQL""").strip()
      assert pre_state == "Failed", f"expected Failed pre-signal, got {pre_state!r}"

      # Step 3: wait for the CP to emit `rollback_signal` on the next
      # CheckinResponse. The `rollback_signal_for_checkin` path logs
      # this line at INFO level on every emission.
      print("step 3: waiting for CP rollback-signal emission…")
      wait_for_journal_match(
          host,
          since_cursor=pre_signal_cursor,
          unit="nixfleet-control-plane.service",
          pattern="rollback-signal: emitting RollbackSignal",
          timeout=60,
          label="CP rollback-signal emission",
      )
      print("step 3: CP emitted rollback-signal as expected")

      # Step 4: agent-side handle_cp_rollback_signal fires. The
      # `agent: CP issued rollback signal` log line is emitted from
      # `agent/dispatch.rs:494` before the rollback is fired.
      print("step 4: waiting for agent-side rollback handling…")
      wait_for_journal_match(
          host,
          since_cursor=pre_signal_cursor,
          unit="microvm@${agentName}.service",
          pattern="CP issued rollback signal",
          timeout=60,
          label="agent rollback-signal handling",
      )
      print("step 4: agent-side rollback fired")

      # Step 5: agent posts RollbackTriggered → CP routes::reports
      # transitions the host_rollout_state row from Failed to
      # Reverted via `apply_rollback_state_transition`, then stamps
      # the terminal state on both the host_dispatch_state
      # operational row (state='rolled-back') and the matching
      # dispatch_history audit row (terminal_state='rolled-back') via
      # `record_terminal`. The operational row stays parked on disk
      # until the next dispatch UPSERTs it;
      # `active_rollouts_snapshot` filters terminal states out so the
      # row no longer surfaces as in-flight.
      print("step 5: waiting for Failed → Reverted transition…")
      wait_for_journal_match(
          host,
          since_cursor=pre_signal_cursor,
          unit="nixfleet-control-plane.service",
          pattern="RollbackTriggered: host_rollout_state Failed . Reverted",
          timeout=60,
          label="Failed → Reverted transition",
      )
      print("step 5: Failed → Reverted transition observed")

      # Step 6: terminal stamps. The operational row's `state` flips
      # to 'rolled-back' and the audit row's `terminal_state` is set
      # the same. Both are best-effort writes in
      # `apply_rollback_state_transition`, so allow a few seconds for
      # the write to land after the journal log line.
      print("step 6: asserting terminal stamps on host_dispatch_state + dispatch_history…")
      stamp_deadline = time.monotonic() + 15
      op_state = ""
      audit_terminal = ""
      # Inline single-line SQL rather than heredoc here — nixosTest
      # dedents the testScript by the outermost indentation level
      # (6 spaces). Heredocs at function-body depth (e.g. the step-2
      # INSERTs above) land at column 0 after dedent, which bash
      # accepts as the closing delimiter; heredocs nested inside this
      # while loop sit at 10-space depth → column 4 after dedent →
      # bash ignores the indented `SQL` and treats the entire heredoc
      # body as content, then sqlite3 fails parsing the literal `SQL`.
      op_q = (
          f"SELECT state FROM host_dispatch_state "
          f"WHERE hostname='${agentName}' "
          f"AND rollout_id='{injected_rollout_id}';"
      )
      audit_q = (
          f"SELECT IFNULL(terminal_state, 'NULL') FROM dispatch_history "
          f"WHERE hostname='${agentName}' "
          f"AND rollout_id='{injected_rollout_id}';"
      )
      while time.monotonic() < stamp_deadline:
          op_state = host.succeed(
              f'sqlite3 /var/lib/nixfleet-cp/state.db "{op_q}"'
          ).strip()
          audit_terminal = host.succeed(
              f'sqlite3 /var/lib/nixfleet-cp/state.db "{audit_q}"'
          ).strip()
          if op_state == "rolled-back" and audit_terminal == "rolled-back":
              break
          time.sleep(2)
      assert op_state == "rolled-back", (
          f"host_dispatch_state.state did not flip to 'rolled-back' "
          f"for {injected_rollout_id}; got {op_state!r}"
      )
      assert audit_terminal == "rolled-back", (
          f"dispatch_history.terminal_state did not stamp 'rolled-back' "
          f"for {injected_rollout_id}; got {audit_terminal!r}"
      )
      print("step 6: terminal stamps observed on both tables")

      # Step 7: idempotency. Once the row is Reverted,
      # compute_rollback_signal returns None — subsequent checkins
      # carry no rollback field. Sample two more poll cycles and
      # assert no fresh emission lines.
      #
      # 2s sleep before cursor capture so journalctl --since (which
      # rounds DOWN to the second) doesn't include the original
      # pre-Reverted rollback-signal emission. Same race teardown.nix
      # mitigates the same way.
      print("step 7: waiting for two more polls + asserting no re-emission…")
      host.succeed("sleep 2")
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
