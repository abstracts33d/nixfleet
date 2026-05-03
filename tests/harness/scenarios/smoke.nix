# tests/harness/scenarios/smoke.nix
#
# Minimal smoke scenario: 2 agent microVMs boot on a host VM that also
# runs a CP stub as a host-level systemd service. Each agent fetches
# /fleet.resolved.json from the CP over mTLS and logs `harness-agent-ok:
# signedAt=...`. Scenario asserts both agents emit the OK marker within 60s.
#
# Why CP-on-host rather than CP-in-microVM: qemu user-mode networking
# isolates every microVM from every other microVM (each VM's gateway
# 10.0.2.2 is the host VM). Running CP on the host VM lets every agent
# microVM reach it via that shared gateway with zero bridge/NAT setup.
# The same placement applies to a future real-CP harness; only the
# systemd unit body in nodes/cp.nix would change.
#
# This is the substrate for every future Checkpoint 2 scenario (magic
# rollback, compliance gate, freshness refusal). When those land, copy
# this file, flip agent config (e.g. inject bad signature into the fixture
# for freshness-refusal), and assert the opposite outcome. The
# signed-fixture flow (build-time-signed canonical.json + verify via the
# `nixfleet-verify-artifact` CLI) is already exercised by the sibling
# `signed-roundtrip` scenario; tamper/refusal twin scenarios should fork
# that file rather than this one.
{
  lib,
  harnessLib,
  testCerts,
  resolvedJsonPath,
  # Fleet-N parameterisation. Default agentNames preserves
  # the original 2-agent smoke shape; the fleet-N wrappers in
  # tests/harness/default.nix override this for fleet-5 / fleet-10.
  agentNames ? ["agent-01" "agent-02"],
  scenarioName ? "fleet-harness-smoke",
  ...
}: let
  cpHostModule = harnessLib.mkCpHostModule {
    inherit testCerts resolvedJsonPath;
  };

  mkAgent = name:
    harnessLib.mkAgentNode {
      inherit testCerts;
      hostName = name;
    };

  agents = lib.listToAttrs (map (n: {
      name = n;
      value = mkAgent n;
    })
    agentNames);
in
  harnessLib.mkFleetScenario {
    name = scenarioName;
    inherit cpHostModule agents;
    timeout = 600;
    testScript = ''
      start_all()

      # Bring the host VM up. The CP stub is a host-VM systemd unit so it
      # comes up with multi-user.target.
      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("harness-cp.service")
      host.wait_for_open_port(8443)

      # microvm.nix launches each agent as `microvm@<name>.service` on
      # the host once microvms.target converges.
      host.wait_for_unit("microvms.target", timeout=300)
      for vm in ${builtins.toJSON agentNames}:
          host.wait_for_unit(f"microvm@{vm}.service", timeout=300)

      # Two-phase timing: gate on guest-side readiness BEFORE starting
      # the activity-budget timer. See `testScriptPrelude` in
      # tests/harness/lib.nix for the full rationale (boot-time
      # variance is host-dependent and unbounded; activity time is
      # fast and predictable). Per-vm timeout is 600s for fleet-10 on
      # constrained hardware; once all guests are ready, the
      # harness-agent oneshot fires within seconds.
      wait_for_microvms_ready(host, ${builtins.toJSON agentNames})

      # Activity phase: the harness-agent oneshot fetches fleet.resolved
      # over mTLS and emits `harness-agent-ok` when the curl + jq
      # parse both succeed. With guests already at multi-user.target
      # this is single-digit seconds; 60s is comfortable headroom for
      # network-online ordering and journal flush.
      deadline = time.monotonic() + 60
      pending = set(${builtins.toJSON agentNames})
      while pending and time.monotonic() < deadline:
          done = set()
          for agent in pending:
              # The microvm logs end up on the host journal tagged with
              # the unit name. Grep for the marker emitted by
              # tests/harness/nodes/agent.nix.
              rc, _ = host.execute(
                  f"journalctl -u microvm@{agent}.service --no-pager "
                  f"| grep -q 'harness-agent-ok:'"
              )
              if rc == 0:
                  done.add(agent)
          pending -= done
          if pending:
              time.sleep(2)

      if pending:
          raise Exception(
              f"agents did not report harness-agent-ok within 60s of "
              f"reaching multi-user: {pending}"
          )

      print("fleet-harness-smoke: all agents fetched fleet.resolved.json over mTLS")
    '';
  }
