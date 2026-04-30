# tests/harness/default.nix
#
# Entry point for the microvm.nix-based fleet simulation harness (issue #5).
#
# Returns an attrset of discoverable scenarios. `flake-module.nix` registers
# these under `checks.<system>.fleet-harness-*`. Each scenario is a
# standalone runNixOSTest derivation so a failure in one doesn't mask
# the others (same convention as modules/tests/_vm-fleet-scenarios/).
#
# Scenario inventory:
# - smoke / fleet-2 / fleet-5 / fleet-10 — stub CP + N stub agents.
#   N=2 is the canonical smoke; N=5 / N=10 satisfy issue #5's
#   "fleet-N" parameterisation.
# - signed-roundtrip — stub CP serving the signed fixture, agent
#   verifies via the verify-artifact CLI.
# - teardown — real CP binary + real agents; wipes CP DB mid-run
#   and asserts state recovery within one reconcile cycle
#   (issue #14, ARCHITECTURE.md §8).
{
  lib,
  pkgs,
  inputs,
  # `nixfleet-canonicalize` is built by the workspace crane pipeline
  # (see `crane-workspace.nix`) and wired in by `modules/tests/harness.nix`.
  # Default to `null` so this file still evaluates from callers that don't
  # pass it — fixture-dependent attrs will throw on access.
  nixfleet-canonicalize ? null,
  # `nixfleet-verify-artifact` is built by the same crane pipeline. The
  # signed-roundtrip scenario invokes it from inside the agent microVM;
  # the smoke scenario does not need it.
  nixfleet-verify-artifact ? null,
  # Real binaries for the teardown scenario (issue #14) and any
  # future scenario that needs real CP / agent semantics (rollouts,
  # dispatch, magic rollback). Defaults to null so callers without
  # the crane workspace still get the stub-based smoke +
  # signed-roundtrip scenarios.
  nixfleet-control-plane ? null,
  nixfleet-agent ? null,
}: let
  harnessLib = import ./lib.nix {inherit lib pkgs inputs;};

  # Hostnames that get test certs minted. Every scenario operates
  # within this set; adding a new agent name to a future fleet-N
  # variant means appending here too.
  certHostnames = [
    "cp"
    "agent-01"
    "agent-02"
    "agent-03"
    "agent-04"
    "agent-05"
    "agent-06"
    "agent-07"
    "agent-08"
    "agent-09"
    "agent-10"
  ];

  sharedCerts = harnessLib.mkHarnessCerts {
    hostnames = certHostnames;
  };

  # Helper that generates ["agent-01" .. "agent-NN"] padded to 2
  # digits (matches the cert-minting hostnames + microvm naming).
  mkAgentNames = n:
    map (i: "agent-${lib.fixedWidthString 2 "0" (toString i)}") (lib.range 1 n);

  scenarioArgs = {
    inherit lib pkgs inputs harnessLib;
    testCerts = sharedCerts;
    resolvedJsonPath = ./fixtures/fleet-resolved.json;
  };

  # Phase 2 PR(a): signed-fixture derivation. Consumed by the
  # `signed-roundtrip` scenario and by `crates/nixfleet-verify-artifact`.
  # See ./fixtures/signed/README.md.
  signedFixture =
    if nixfleet-canonicalize == null
    then
      throw ''
        tests/harness: signedFixture requires `nixfleet-canonicalize` to be
        passed in. Wire it via `modules/tests/harness.nix` or call sites
        that have the flake's `packages.<system>.nixfleet-canonicalize`.
      ''
    else
      import ./fixtures/signed {
        inherit lib pkgs nixfleet-canonicalize;
      };

  # Encrypted-secret fixture for the secret-hygiene scenario. Outputs
  # an age-encrypted blob + matching identity + the plaintext bytes
  # the scenario greps for absence of in CP-side disk dumps.
  agenixFixture = import ./fixtures/agenix {inherit pkgs;};

  # Pre-signed probe-output fixture for the auditor-chain scenario.
  # Outputs canonical payload + base64 sig + OpenSSH pubkey; consumed
  # by verify-artifact probe.
  probeFixture =
    if nixfleet-canonicalize == null
    then null
    else import ./fixtures/probe {inherit pkgs nixfleet-canonicalize;};

  # Stale variant of the signed fixture (issue #13): deliberately
  # signs the artifact a year and a half in the past so any sane
  # freshness window puts the agent's clock-skewed `now − signedAt`
  # well past `freshness_window + 60s`. Used by the
  # stale-target-refusal scenario; CP runs with `--freshness-window-secs`
  # large enough to accept the artifact (so it dispatches), and the
  # agent's per-channel freshness check fires first.
  staleFixture =
    if nixfleet-canonicalize == null
    then null
    else
      import ./fixtures/signed {
        inherit lib pkgs nixfleet-canonicalize;
        signedAt = "2025-01-01T00:00:00Z";
        # Smallest mk-fleet-permissible window (2 × signingInterval=60).
        freshnessWindowMinutes = 120;
        seedSalt = "nixfleet-harness-stale-fixture-2025";
        derivationName = "nixfleet-harness-stale-fixture";
      };

  # Phase 2 PR(b): signed-roundtrip scenario. Depends on both
  # `signedFixture` (fixture bytes + trust.json) and
  # `nixfleet-verify-artifact` (the CLI the agent microVM runs).
  signedRoundtripScenario =
    if nixfleet-verify-artifact == null
    then
      throw ''
        tests/harness: fleet-harness-signed-roundtrip requires
        `nixfleet-verify-artifact` to be passed in. Wire it via
        `modules/tests/harness.nix` using the crane-built package.
      ''
    else
      import ./scenarios/signed-roundtrip.nix (scenarioArgs
        // {
          inherit signedFixture;
          verifyArtifactPkg = nixfleet-verify-artifact;
        });

  # Cycle-N+1 teardown scenario (issue #14). Real CP + real agents;
  # wipes CP DB mid-run; asserts agents repopulate within one
  # reconcile cycle.
  teardownScenario =
    if nixfleet-control-plane == null || nixfleet-agent == null
    then
      throw ''
        tests/harness: fleet-harness-teardown requires both
        `nixfleet-control-plane` and `nixfleet-agent` to be passed
        in. Wire via `modules/tests/harness.nix` using the crane-
        built packages.
      ''
    else
      import ./scenarios/teardown.nix (scenarioArgs
        // {
          inherit signedFixture;
          cpPkg = nixfleet-control-plane;
          agentPkg = nixfleet-agent;
        });

  # Issue #13 stale-target refusal scenario. Real CP serving a
  # year-and-a-half-old fixture (CP accepts because its
  # --freshness-window-secs is bumped huge); real agent receives the
  # dispatched target, the `nixfleet_agent::freshness` gate fires
  # because the channel's per-fleet freshness_window is much smaller
  # than `now − signedAt`, agent posts `ReportEvent::StaleTarget`
  # and skips activation.
  staleTargetScenario =
    if nixfleet-control-plane == null || nixfleet-agent == null || staleFixture == null
    then
      throw ''
        tests/harness: fleet-harness-stale-target requires
        `nixfleet-control-plane`, `nixfleet-agent`, and
        `nixfleet-canonicalize` (for staleFixture) to be passed in.
      ''
    else
      import ./scenarios/stale-target.nix (scenarioArgs
        // {
          staleFixture = staleFixture;
          cpPkg = nixfleet-control-plane;
          agentPkg = nixfleet-agent;
        });

  # ADR-011 boot-recovery scenario. Pre-stages a stale `last_dispatched`
  # file on the agent microVM, asserts the agent's check_boot_recovery
  # path clears it before the regular poll loop. Exercises the
  # StaleClearedMismatch branch (Acknowledged path is unit-tested in
  # crates/nixfleet-agent/src/recovery.rs::tests).
  bootRecoveryScenario =
    if nixfleet-control-plane == null || nixfleet-agent == null
    then
      throw ''
        tests/harness: fleet-harness-boot-recovery requires both
        `nixfleet-control-plane` and `nixfleet-agent` to be passed in.
      ''
    else
      import ./scenarios/boot-recovery.nix (scenarioArgs
        // {
          inherit signedFixture;
          cpPkg = nixfleet-control-plane;
          agentPkg = nixfleet-agent;
        });

  # Auditor offline-chain scenario. Pure runCommand — the verify
  # path is offline by definition, no microvm required.
  auditorChainScenario =
    if nixfleet-verify-artifact == null || probeFixture == null
    then
      throw ''
        tests/harness: fleet-harness-auditor-chain requires both
        `nixfleet-canonicalize` (for probeFixture) and
        `nixfleet-verify-artifact`. Wire via modules/tests/harness.nix.
      ''
    else
      import ./scenarios/auditor-chain.nix {
        inherit pkgs probeFixture;
        verifyArtifactPkg = nixfleet-verify-artifact;
      };

  # Issue #2 step 5 deadline-expiry scenario. Real CP with a 3-second
  # confirm deadline; testScript drives the wire flow via curl from
  # the host VM (no agent microVM needed) — POST checkin → receive
  # target → wait past deadline → POST confirm → assert HTTP 410.
  # Validates the rollback_timer + handlers.rs:880-898 path.
  deadlineExpiryScenario =
    if nixfleet-control-plane == null
    then
      throw ''
        tests/harness: fleet-harness-deadline-expiry requires
        `nixfleet-control-plane` to be passed in.
      ''
    else
      import ./scenarios/deadline-expiry.nix (scenarioArgs
        // {
          inherit signedFixture;
          cpPkg = nixfleet-control-plane;
        });

  # Helper for the parameterised fleet-N variants — same as smoke
  # but with N agents under the same stub-CP + stub-agent scaffolding.
  mkFleetNScenario = n:
    import ./scenarios/smoke.nix (scenarioArgs
      // {
        agentNames = mkAgentNames n;
        scenarioName = "fleet-harness-fleet-${toString n}";
      });
in {
  fleet-harness-smoke = import ./scenarios/smoke.nix scenarioArgs;

  fleet-harness-signed-roundtrip = signedRoundtripScenario;

  fleet-harness-teardown = teardownScenario;

  fleet-harness-stale-target = staleTargetScenario;

  fleet-harness-boot-recovery = bootRecoveryScenario;

  fleet-harness-deadline-expiry = deadlineExpiryScenario;

  fleet-harness-auditor-chain = auditorChainScenario;

  # Issue #5 fleet-N variants. fleet-2 is identical to smoke
  # under a different name, kept for criterion completeness.
  fleet-harness-fleet-2 = mkFleetNScenario 2;
  fleet-harness-fleet-5 = mkFleetNScenario 5;
  fleet-harness-fleet-10 = mkFleetNScenario 10;

  # Signed-fixture derivation exposed as a harness attribute. Registered
  # as a flake check (`signed-fixture`) in `modules/tests/harness.nix`
  # so byte-stability regressions surface on every CI run.
  inherit signedFixture;
  inherit agenixFixture;
  inherit probeFixture;
}
