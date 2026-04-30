# Tier A - microvm.nix-based fleet simulation harness.
#
# Registers `checks.x86_64-linux.fleet-harness-*` discoverable scenarios.
# Each scenario boots one CP microVM + N agent microVMs on a single host
# VM, with /nix/store shared over virtiofs for near-zero cold-start cost.
#
# DIFFERENT from modules/tests/vm-fleet-scenarios.nix: that file wires
# full-closure agent/CP nodes through pkgs.testers.runNixOSTest with
# nothing microvm-related. The harness here uses astro/microvm.nix guests.
# Do NOT unify the two substrates - they solve different problems.
#
# Run (once the user is ready):
#   nix build .#checks.x86_64-linux.fleet-harness-smoke --no-link
{inputs, ...}: {
  perSystem = {
    pkgs,
    lib,
    system,
    config,
    ...
  }: let
    # Pull crane-built packages from the workspace (same perSystem,
    # declared in `modules/rust-packages.nix`). The harness entry point
    # uses `nixfleet-canonicalize` to bake the signed fixture and
    # `nixfleet-verify-artifact` as the binary the signed-roundtrip
    # agent microVM runs.
    nixfleet-canonicalize = config.packages.nixfleet-canonicalize or null;
    nixfleet-verify-artifact = config.packages.nixfleet-verify-artifact or null;
    # Real binaries for the teardown scenario. Static-
    # fixture stub nodes (cp.nix / agent.nix / cp-signed.nix) keep
    # working with the existing scenarios; the teardown scenario opts
    # into the real-binary nodes via these.
    nixfleet-control-plane = config.packages.nixfleet-control-plane or null;
    nixfleet-agent = config.packages.nixfleet-agent or null;
    harness = import ../../tests/harness {
      inherit lib pkgs inputs nixfleet-canonicalize nixfleet-verify-artifact;
      inherit nixfleet-control-plane nixfleet-agent;
    };
  in
    lib.optionalAttrs (system == "x86_64-linux") {
      checks =
        {
          fleet-harness-smoke = harness.fleet-harness-smoke;
        }
        # Only register the signed-fixture check when the canonicalize
        # package is available for this system (x86_64-linux only today;
        # other systems skip it silently).
        // lib.optionalAttrs (nixfleet-canonicalize != null) {
          # Signed-fixture derivation. Byte-stability regression guard;
          # rebuild failure signals non-determinism in mkFleet,
          # canonicalize, or the keygen helper.
          signed-fixture = harness.signedFixture;

          # Signed `revocations.json` sidecar fixture exposed
          # standalone — same byte-stability role as signed-fixture.
          revocations-fixture = harness.revocationsFixture;
        }
        // lib.optionalAttrs (nixfleet-canonicalize != null && nixfleet-verify-artifact != null) {
          # Signed-roundtrip scenario. Exercises the full stack:
          # fixture build -> mTLS serve -> agent fetch ->
          # verify_artifact accept -> OK marker.
          fleet-harness-signed-roundtrip = harness.fleet-harness-signed-roundtrip;

          # Auditor offline-chain scenario. Validates that
          # verify-artifact probe accepts a well-formed signed probe
          # payload and rejects a byte-flipped copy.
          fleet-harness-auditor-chain = harness.fleet-harness-auditor-chain;

          # Corruption-rejection scenario. Bit-flips the signed
          # fixture's canonical bytes and signature; asserts
          # verify-artifact rejects each.
          fleet-harness-corruption-rejection = harness.fleet-harness-corruption-rejection;

          # Probe-output fixture exposed standalone — byte-stability
          # regression guard, same role as signed-fixture.
          probe-fixture = harness.probeFixture;
        }
        // lib.optionalAttrs (
          nixfleet-canonicalize
          != null
          && nixfleet-control-plane != null
          && nixfleet-agent != null
        ) {
          # Teardown scenario. Real CP + real agents;
          # wipes the CP DB mid-run and asserts state recovery
          # within one reconcile cycle.
          fleet-harness-teardown = harness.fleet-harness-teardown;

          # Confirm-deadline expiry → 410.
          # Host-side curl drives the wire flow against cp-real
          # with --confirm-deadline-secs 3.
          fleet-harness-deadline-expiry = harness.fleet-harness-deadline-expiry;

          # Agent-side freshness gate wire-format. CP
          # serves a year-and-a-half-old fixture; testScript asserts
          # dispatched targets carry signedAt + freshnessWindowSecs
          # such that the agent's freshness::check returns Stale.
          fleet-harness-stale-target = harness.fleet-harness-stale-target;

          # ADR-011 boot-recovery: agent boots with a pre-staged
          # stale last_dispatched file; check_boot_recovery clears
          # it before the regular poll loop fires.
          fleet-harness-boot-recovery = harness.fleet-harness-boot-recovery;

          # Parameterised fleet-N variants. Same
          # scenario as fleet-harness-smoke but with N agents.
          # CI runs fleet-2 on PR; fleet-10 / fleet-50 are
          # available for nightly / on-demand.
          fleet-harness-fleet-2 = harness.fleet-harness-fleet-2;
          fleet-harness-fleet-5 = harness.fleet-harness-fleet-5;
          fleet-harness-fleet-10 = harness.fleet-harness-fleet-10;
        };
    };
}
