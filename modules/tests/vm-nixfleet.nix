# Tier A — VM integration test: NixFleet agent ↔ control plane cycle.
#
# Two-node nixosTest proving the full systemd service lifecycle:
#   1. Control plane starts and listens on port 8080
#   2. Agent starts and polls the control plane
#   3. Operator sets a desired generation via the CP API
#   4. Agent detects mismatch, runs dry-run cycle, reports back
#   5. CP inventory reflects the agent's report
#
# Run: nix build .#checks.x86_64-linux.vm-nixfleet --no-link
{
  inputs,
  config,
  ...
}: {
  perSystem = {
    pkgs,
    system,
    lib,
    ...
  }: let
    helpers = import ./_lib/helpers.nix {inherit lib;};

    mkTestNode = helpers.mkTestNode {
      inherit inputs;
      nixosModules = builtins.attrValues config.flake.modules.nixos;
      hmModules = builtins.attrValues config.flake.modules.homeManager;
      hmLinuxModules = builtins.attrValues config.flake.modules.hmLinux;
      hostSpecModule = ../_shared/host-spec-module.nix;
    };

    defaultTestSpec = helpers.defaultTestSpec;
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks = {
        # --- vm-nixfleet: agent ↔ control plane end-to-end cycle ---
        vm-nixfleet = pkgs.testers.nixosTest {
          name = "vm-nixfleet";

          nodes.cp = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                hostName = "cp";
              };
            extraModules = [
              {
                services.nixfleet-control-plane = {
                  enable = true;
                  openFirewall = true;
                };
              }
            ];
          };

          nodes.agent = mkTestNode {
            hostSpecValues =
              defaultTestSpec
              // {
                hostName = "agent";
              };
            extraModules = [
              {
                services.nixfleet-agent = {
                  enable = true;
                  controlPlaneUrl = "http://cp:8080";
                  machineId = "agent";
                  pollInterval = 2;
                  dryRun = true;
                };
              }
            ];
          };

          testScript = ''
            import json

            # 1. Start control plane and wait for readiness
            cp.start()
            cp.wait_for_unit("nixfleet-control-plane.service")
            cp.wait_for_open_port(8080)

            # 2. Start agent and wait for its service
            agent.start()
            agent.wait_for_unit("nixfleet-agent.service")

            # 3. Set a desired generation for the agent machine via CP API.
            #    Use a fake store path -- the agent will detect a mismatch with
            #    its real /run/current-system and enter the fetch/dry-run path.
            cp.succeed(
                "curl -sf -X POST "
                "http://localhost:8080/api/v1/machines/agent/set-generation "
                "-H 'Content-Type: application/json' "
                "-d '{\"hash\": \"/nix/store/fake-test-generation\"}'"
            )

            # 4. Wait for the agent to poll, detect mismatch, and report back.
            #    Agent cycle: Idle (2s sleep) -> Checking (reads /run/current-system,
            #    compares with desired) -> Fetching (no cache_url = no-op) ->
            #    dry-run branch -> Reporting (POST /report with success=true,
            #    message="dry-run: would apply") -> Idle.
            #    After report, the CP inventory will show "agent" in machine list.
            cp.wait_until_succeeds(
                "curl -sf http://localhost:8080/api/v1/machines | grep '\"machine_id\"'",
                timeout=30,
            )

            # 5. Verify the machine inventory contains the agent with expected state
            result = cp.succeed("curl -sf http://localhost:8080/api/v1/machines")
            machines = json.loads(result)

            agent_entry = None
            for m in machines:
                if m["machine_id"] == "agent":
                    agent_entry = m
                    break

            assert agent_entry is not None, f"Agent not found in inventory: {machines}"

            # dry-run reports success=true, which maps to system_state "ok"
            assert agent_entry["system_state"] == "ok", (
                f"Expected system_state 'ok' (dry-run reports success), "
                f"got: {agent_entry['system_state']}"
            )

            # The desired generation should be the fake hash we set
            assert agent_entry["desired_generation"] == "/nix/store/fake-test-generation", (
                f"Expected desired_generation '/nix/store/fake-test-generation', "
                f"got: {agent_entry['desired_generation']}"
            )
          '';
        };
      };
    };
}
