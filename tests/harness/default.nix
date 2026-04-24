# tests/harness/default.nix
#
# Entry point for the microvm.nix-based fleet simulation harness (issue #5).
#
# Returns an attrset of discoverable scenarios. `flake-module.nix` registers
# these under `checks.<system>.fleet-harness-*`. Each scenario is a
# standalone runNixOSTest derivation so a failure in one doesn't mask
# the others (same convention as modules/tests/_vm-fleet-scenarios/).
#
# Scaffold scope: one scenario (`smoke`, N=2 agents). The extension path
# for `fleet-N` (the acceptance target from issue #5) is to import a new
# scenario file here and parameterise the agent count — see scenarios/smoke.nix
# for the pattern.
{
  lib,
  pkgs,
  inputs,
}: let
  harnessLib = import ./lib.nix {inherit lib pkgs inputs;};

  # One shared cert set for every harness scenario. When a new scenario
  # needs a new hostname, append it here and it's available to every
  # scenario without rebuilding the others.
  sharedCerts = harnessLib.mkHarnessCerts {
    hostnames = ["cp" "agent-01" "agent-02"];
  };

  scenarioArgs = {
    inherit lib pkgs inputs harnessLib;
    testCerts = sharedCerts;
    resolvedJsonPath = ./fixtures/fleet-resolved.json;
  };
in {
  # Target shape per issue #5: `checks.<system>.fleet-N`. For the scaffold
  # we only ship N=2 (smoke). Extension: import additional scenario files
  # here with different agent counts, or parameterise smoke.nix to accept
  # `agentCount` and expose fleet-5, fleet-10 wrappers.
  fleet-harness-smoke = import ./scenarios/smoke.nix scenarioArgs;
}
