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

  # For N > 2 (fleet-5 / fleet-10), parallel cold-boot of every
  # microvm guest saturates I/O on commodity lab hardware — DHCP under
  # qemu user-net stops converging and `harness-agent.service` hangs
  # in ExecStartPre's route-wait. Stagger starts to spread the load.
  isFleetN = builtins.length agentNames > 2;
  staggerSecs = 8;
in
  harnessLib.mkFleetScenario {
    name = scenarioName;
    inherit cpHostModule agents;
    # When staggering, disable autostart so the testScript drives
    # microvm@<name>.service start order with sleeps between.
    agentVmAutostart = !isFleetN;
    # Scales with N: fleet-10's marker-wait deadline alone is 570s
    # (max(240, 120 + 45*N)), and there's host-VM + microvms.target +
    # individual microvm@<n>.service waits ahead of that. 1200s
    # ceiling gives headroom on slow lab hardware while still
    # aborting on a real hang.
    timeout = 1200;
    testScript = ''
      start_all()

      # Bring the host VM up. The CP stub is a host-VM systemd unit so it
      # comes up with multi-user.target.
      host.wait_for_unit("multi-user.target")
      host.wait_for_unit("harness-cp.service")
      host.wait_for_open_port(8443)

      ${
        if isFleetN
        then ''
          # Staggered manual start: at fleet-5+ on commodity lab
          # hardware, parallel microvm cold-boot saturates virtiofsd
          # + qemu user-net DHCP and guests never get a default
          # route. Start each microvm@<n>.service with a
          # ${toString staggerSecs}s gap so each guest's network
          # comes up before the next contends. autostart=false on
          # the host config disables the default
          # microvms.target-driven parallel start.
          print("staggered start: launching ${toString (builtins.length agentNames)} microvms with ${toString staggerSecs}s gap")
          for idx, vm in enumerate(${builtins.toJSON agentNames}):
              host.execute(f"systemctl start --no-block microvm@{vm}.service")
              if idx < len(${builtins.toJSON agentNames}) - 1:
                  time.sleep(${toString staggerSecs})
          for vm in ${builtins.toJSON agentNames}:
              host.wait_for_unit(f"microvm@{vm}.service", timeout=300)
        ''
        else ''
          # microvm.nix launches each agent as `microvm@<name>.service` on
          # the host once microvms.target converges.
          host.wait_for_unit("microvms.target", timeout=300)
          for vm in ${builtins.toJSON agentNames}:
              host.wait_for_unit(f"microvm@{vm}.service", timeout=300)
        ''
      }

      # Budget covers BOTH cold boot and activity. The agent units are
      # oneshot+RemainAfterExit; success == one successful mTLS fetch
      # of the fixture. Scales with agent count because mass-booting
      # microVMs on a single host VM serialises on qemu start, guest
      # kernel cold-boot, and the curl that depends on a working
      # default route.
      #
      # `max(300, 150 + 60*N)` is empirical: covers commodity Linux
      # lab hardware where cloud-hypervisor guests take ~30-60s to
      # reach the login banner, with extra headroom for fleet-10
      # where staggered start (8s × (N-1) gaps) further extends the
      # window before the last agent's deadline. Generous
      # over-provisioning is fine — the deadline is the *upper
      # bound*, the loop short-circuits as soon as every agent
      # posts the marker.
      deadline = time.monotonic() + max(300, 150 + 60 * len(${builtins.toJSON agentNames}))
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
          budget = max(300, 150 + 60 * len(${builtins.toJSON agentNames}))
          raise Exception(f"agents did not report harness-agent-ok within {budget}s: {pending}")

      print("fleet-harness-smoke: all agents fetched fleet.resolved.json over mTLS")
    '';
  }
