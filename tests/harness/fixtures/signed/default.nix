# Deterministic signed-fixture derivation for the microvm harness.
#
# Produces an ed25519-signed `fleet.resolved` artifact + matching
# `test-trust.json` (per docs/trust-root-flow.md §3.4) at build time.
# Output: canonical.json, canonical.json.sig, test-trust.json,
# verify-pubkey.b64.
#
# Determinism. Every byte in the output is a pure function of this
# file's inputs: hand-authored fleet declaration below, hardcoded
# `meta.{signedAt, ciCommit, signatureAlgorithm}`, and a 32-byte
# ed25519 seed derived from `seedSalt`. Signing path (canonicalize →
# sign) is factored into ./sign-bytes.nix so future signed sidecars
# (revocations.json, signed probe outputs) reuse the same key + verify
# under the same trust file.
{
  lib,
  pkgs,
  nixfleet-canonicalize,
  mkFleetPath ? ../../../../lib/mk-fleet.nix,
  signedAt ? "2026-05-01T00:00:00Z",
  freshnessWindowMinutes ? 86400,
  seedSalt ? "nixfleet-harness-test-seed-2026",
  derivationName ? "nixfleet-harness-signed-fixture",
  # Per-host closureHash overrides. Default empty → mk-fleet's
  # `closureHash = null` semantics (the production release pipeline
  # injects real values from `system.build.toplevel`). Pass
  # `{ "agent-01" = "..."; }` to make the agent's reported
  # closure_hash match for convergence-dependent scenarios (e.g.
  # the teardown's soak-state attestation recovery proof).
  hostClosureHashes ? {},
}: let
  fixedSignedAt = signedAt;
  fixedCiCommit = "0000000000000000000000000000000000000000";
  fixedAlgorithm = "ed25519";

  # Stub nixosConfiguration: satisfies mkFleet's invariant that each
  # host carries `config.system.build.toplevel`.
  stubConfiguration = {
    config.system.build.toplevel = {
      outPath = "/nix/store/0000000000000000000000000000000000000000-stub";
      drvPath = "/nix/store/0000000000000000000000000000000000000000-stub.drv";
    };
  };

  mkFleetImpl = import mkFleetPath {inherit lib;};
  inherit (mkFleetImpl) mkFleet withSignature;

  # Hand-authored fleet declaration. Two hosts, one channel, one
  # rollout policy. Deliberately minimal so any verify failure is
  # wire-up, not fleet-shape.
  fleetInput = {
    hosts = {
      agent-01 = {
        system = "x86_64-linux";
        configuration = stubConfiguration;
        tags = ["harness"];
        channel = "stable";
        pubkey = null;
      };
      agent-02 = {
        system = "x86_64-linux";
        configuration = stubConfiguration;
        tags = ["harness"];
        channel = "stable";
        pubkey = null;
      };
      cp = {
        system = "x86_64-linux";
        configuration = stubConfiguration;
        tags = ["harness" "control-plane"];
        channel = "stable";
        pubkey = null;
      };
    };
    channels.stable = {
      description = "Harness signed-fixture channel.";
      rolloutPolicy = "all-at-once";
      reconcileIntervalMinutes = 30;
      signingIntervalMinutes = 60;
      freshnessWindow = freshnessWindowMinutes;
      compliance = {
        mode = "permissive";
        frameworks = [];
      };
    };
    rolloutPolicies.all-at-once = {
      strategy = "all-at-once";
      waves = [
        {
          selector.all = true;
          soakMinutes = 0;
        }
      ];
      healthGate = {};
      onHealthFailure = "halt";
    };
    edges = [];
    disruptionBudgets = [];
  };

  fleet = mkFleet fleetInput;

  resolvedWithClosureHashes =
    fleet.resolved
    // {
      hosts =
        lib.mapAttrs (name: host:
          host
          // (lib.optionalAttrs (hostClosureHashes ? ${name}) {
            closureHash = hostClosureHashes.${name};
          }))
        fleet.resolved.hosts;
    };

  stamped =
    withSignature {
      signedAt = fixedSignedAt;
      ciCommit = fixedCiCommit;
      signatureAlgorithm = fixedAlgorithm;
    }
    resolvedWithClosureHashes;

  signed = import ./sign-bytes.nix {
    inherit pkgs nixfleet-canonicalize seedSalt;
    name = "${derivationName}-signed";
    jsonContent = builtins.toJSON stamped;
  };
  # `now` for verify-artifact / agent-verify consumers: signedAt + 1h.
  # All harness signedAt values land at `T00:00:00Z`, so a literal
  # string replace is correct + assertion-checked. Anything else
  # would need a chrono-style parser, which Nix lacks; if a future
  # fixture overrides signedAt with non-midnight, this assert fires
  # rather than silently producing the wrong now.
  signedAtMidnightSuffix = "T00:00:00Z";
  signedAtPlusHourSuffix = "T01:00:00Z";
  now =
    assert lib.hasSuffix signedAtMidnightSuffix signedAt;
      lib.removeSuffix signedAtMidnightSuffix signedAt + signedAtPlusHourSuffix;
in
  pkgs.runCommand derivationName {
    # Expose the build-time stamps so consumers can derive a `now`
    # value for verify-artifact / agent-verify without coupling-by-
    # comment to the literal in this file.
    passthru = {inherit signedAt now;};
  } ''
    set -euo pipefail
    mkdir -p "$out"
    cp ${signed}/canonical.json "$out/canonical.json"
    cp ${signed}/canonical.json.sig "$out/canonical.json.sig"
    cp ${signed}/pubkey.b64 "$out/verify-pubkey.b64"

    pubkey_b64=$(cat ${signed}/pubkey.b64)
    cat > "$out/test-trust.json" <<EOF
    {
      "schemaVersion": 1,
      "ciReleaseKey": {
        "current": { "algorithm": "ed25519", "public": "$pubkey_b64" },
        "previous": null,
        "rejectBefore": null
      },
      "cacheKeys": [],
      "orgRootKey": { "current": null }
    }
    EOF
  ''
