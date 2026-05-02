# tests/harness/default.nix
#
# Entry point for the microvm.nix-based fleet simulation harness.
#
# Returns an attrset of discoverable scenarios. `flake-module.nix` registers
# these under `checks.<system>.fleet-harness-*`. Each scenario is a
# standalone runNixOSTest derivation so a failure in one doesn't mask
# the others (same convention as modules/tests/_vm-fleet-scenarios/).
#
# Scenario inventory:
# - smoke / fleet-2 / fleet-5 / fleet-10 — stub CP + N stub agents.
#   N=2 is the canonical smoke; N=5 / N=10 satisfy the
#   "fleet-N" parameterisation.
# - signed-roundtrip — stub CP serving the signed fixture, agent
#   verifies via the verify-artifact CLI.
# - teardown — real CP binary + real agents; wipes CP DB mid-run
#   and asserts state recovery within one reconcile cycle
#   (ARCHITECTURE.md §8).
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
  # Real binaries for the teardown scenario and any
  # future scenario that needs real CP / agent semantics (rollouts,
  # dispatch, magic rollback). Defaults to null so callers without
  # the crane workspace still get the stub-based smoke +
  # signed-roundtrip scenarios.
  nixfleet-control-plane ? null,
  nixfleet-agent ? null,
  # Operator-side helper binaries (`nixfleet-mint-token`,
  # `nixfleet-derive-pubkey`). Used by the enroll-replay scenario
  # to mint bootstrap tokens at runtime inside the host VM. Default
  # null so callers without the crane workspace still get the
  # other scenarios.
  nixfleet-cli ? null,
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

  # Org-root ed25519 keypair fixture for the enroll-replay scenario
  # (and any future enrolment-flow scenario). The trust.json shipped
  # alongside wires `orgRootKey.current` (so the CP's enrol handler
  # accepts tokens signed by this keypair). Stitched together with
  # the signedFixture's pubkey at runCommand-time so the same trust
  # file ALSO carries `ciReleaseKey.current`, letting the CP boot
  # against the harness's signed fleet bytes without two trust
  # files. The signed-fixture's pubkey is read from a derivation
  # path inside the runCommand, not at Nix eval time.
  orgRootKeyFixture =
    if nixfleet-canonicalize == null
    then null
    else let
      # Standalone keypair (no trust.json embedded). The combined
      # trust.json is built by the runCommand below.
      bareKey = import ./fixtures/org-root-key {
        inherit pkgs;
      };
    in
      pkgs.runCommand "nixfleet-harness-org-root-key-with-trust" {} ''
        set -euo pipefail
        mkdir -p "$out"
        cp ${bareKey}/private.pem "$out/private.pem"
        cp ${bareKey}/pubkey.b64 "$out/pubkey.b64"
        org_pub=$(cat ${bareKey}/pubkey.b64)
        ci_pub=$(cat ${signedFixture}/verify-pubkey.b64)
        cat > "$out/trust.json" <<EOF
        {
          "schemaVersion": 1,
          "ciReleaseKey": {
            "current": { "algorithm": "ed25519", "public": "$ci_pub" },
            "previous": null,
            "rejectBefore": null
          },
          "cacheKeys": [],
          "orgRootKey": {
            "current": { "algorithm": "ed25519", "public": "$org_pub" },
            "previous": null,
            "rejectBefore": null
          }
        }
        EOF
      '';

  # Pre-signed probe-output fixture for the auditor-chain scenario.
  # Outputs canonical payload + base64 sig + OpenSSH pubkey; consumed
  # by verify-artifact probe.
  probeFixture =
    if nixfleet-canonicalize == null
    then null
    else import ./fixtures/probe {inherit pkgs nixfleet-canonicalize;};

  # Pre-signed rollout-manifest fixture for the manifest-tamper-rejection
  # scenario. Outputs canonical manifest + raw sig + pubkey + trust.json
  # + the rolloutId (sha256 of canonical bytes). Consumed by
  # `nixfleet-verify-artifact rollout-manifest`.
  rolloutManifestFixture =
    if nixfleet-canonicalize == null
    then null
    else import ./fixtures/rollout-manifest {inherit pkgs nixfleet-canonicalize;};

  # Signed `revocations.json` sidecar — verifies under the same
  # test-trust.json as signedFixture (shared seedSalt). Consumed by
  # the teardown scenario to assert hard-state replay after CP wipe.
  revocationsFixture =
    if nixfleet-canonicalize == null
    then null
    else import ./fixtures/signed/revocations.nix {inherit pkgs nixfleet-canonicalize;};

  # Convergence variant of signedFixture. Same seedSalt → same
  # trust.json verifies it; the difference is an injected per-host
  # closureHash so the agent's reported
  # current_generation.closure_hash matches the fleet's expectation
  # (the agent VM overrides /run/current-system to a path with this
  # basename via `harnessLib.convergencePreseedModule`). That match
  # is what the CP-side soak-state attestation recovery requires
  # before applying last_confirmed_at — and any future scenario that
  # wants to assert convergence-gated behaviour needs it too. The
  # plain `signedFixture` leaves closureHash null per host, which
  # silently produces `Decision::NoDeclaration` on dispatch; using
  # this variant up-front prevents that latent false-pass.
  convergedClosureHash = "0000000000000000000000000000000000000000-harness-stub";
  convergedSignedFixture =
    if nixfleet-canonicalize == null
    then null
    else
      import ./fixtures/signed {
        inherit lib pkgs nixfleet-canonicalize;
        derivationName = "nixfleet-harness-converged-signed-fixture";
        hostClosureHashes = {
          "agent-01" = convergedClosureHash;
          "agent-02" = convergedClosureHash;
        };
      };

  # Stale variant of the signed fixture: deliberately
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

  # Cycle-N+1 teardown scenario. Real CP + real agents;
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
          # Teardown uses the convergence variant of the signed
          # fixture so the agent's reported closure_hash matches the
          # fleet's declared value (lets the soak-state recovery
          # path actually run after the CP wipe).
          signedFixture = convergedSignedFixture;
          inherit revocationsFixture;
          closureHash = convergedClosureHash;
          cpPkg = nixfleet-control-plane;
          agentPkg = nixfleet-agent;
        });

  # Secret-hygiene scenario. Agent decrypts an age-encrypted blob
  # at boot, talks to CP normally; testScript greps every CP-side
  # artifact for the plaintext and asserts no leaks.
  secretHygieneScenario =
    if nixfleet-control-plane == null || nixfleet-agent == null
    then
      throw ''
        tests/harness: fleet-harness-secret-hygiene requires both
        `nixfleet-control-plane` and `nixfleet-agent`. Wire via
        modules/tests/harness.nix.
      ''
    else
      import ./scenarios/secret-hygiene.nix (scenarioArgs
        // {
          # Use the convergence variant + matching preseed so the
          # agent reaches a steady state with its reported closure
          # matching the fleet's declared value. Today the scenario's
          # assertions don't depend on convergence, but applying it
          # pre-emptively eliminates the silent-false-pass class:
          # if a future assertion adds a "wait for convergence" gate,
          # it'll progress instead of early-exiting on NoDeclaration.
          signedFixture = convergedSignedFixture;
          closureHash = convergedClosureHash;
          inherit agenixFixture;
          cpPkg = nixfleet-control-plane;
          agentPkg = nixfleet-agent;
        });

  # Stale-target refusal scenario. Real CP serving a
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
          # Same rationale as secret-hygiene: use the converged
          # fixture + preseed so any future convergence-gated
          # assertion can't silently early-exit on NoDeclaration.
          signedFixture = convergedSignedFixture;
          closureHash = convergedClosureHash;
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

  # Corruption-rejection scenario. Pure runCommand — bit-flips the
  # signed fixture's canonical bytes and signature in turn, asserts
  # verify-artifact rejects each.
  corruptionRejectionScenario =
    if nixfleet-verify-artifact == null
    then
      throw ''
        tests/harness: fleet-harness-corruption-rejection requires
        `nixfleet-verify-artifact`. Wire via modules/tests/harness.nix.
      ''
    else
      import ./scenarios/corruption-rejection.nix {
        inherit pkgs signedFixture;
        verifyArtifactPkg = nixfleet-verify-artifact;
      };

  # Future-dated rejection scenario. Pure runCommand — drives
  # verify-artifact's `--now` flag around the fixture's fixed
  # signedAt to assert symmetric slack behaviour
  # (reject Δ=+2d, accept ±30s, accept 0).
  futureDatedRejectionScenario =
    if nixfleet-verify-artifact == null
    then
      throw ''
        tests/harness: fleet-harness-future-dated-rejection requires
        `nixfleet-verify-artifact`. Wire via modules/tests/harness.nix.
      ''
    else
      import ./scenarios/future-dated-rejection.nix {
        inherit pkgs signedFixture;
        verifyArtifactPkg = nixfleet-verify-artifact;
      };

  # Enroll-replay race scenario. cp-real + non-mTLS curls firing
  # two parallel POSTs to /v1/enroll with the same nonce; asserts
  # exactly one 200 + one 409 (the AlreadyRecorded branch of the
  # token_replay PRIMARY KEY race), one row in token_replay, and
  # the operator-readable log line. Edge case: stops CP, drops
  # the table, asserts a fresh enroll fails closed (not 200).
  enrollReplayScenario =
    if nixfleet-control-plane == null || nixfleet-cli == null || orgRootKeyFixture == null
    then
      throw ''
        tests/harness: fleet-harness-enroll-replay requires
        `nixfleet-control-plane`, `nixfleet-cli`, and
        `nixfleet-canonicalize` (for the org-root key fixture).
        Wire via modules/tests/harness.nix.
      ''
    else
      import ./scenarios/enroll-replay.nix (scenarioArgs
        // {
          inherit signedFixture orgRootKeyFixture;
          cpPkg = nixfleet-control-plane;
          cliPkg = nixfleet-cli;
        });

  # Manifest-tamper-rejection scenario. Pure runCommand — exercises
  # the rollout-manifest leg of the offline auditor chain (RFC-0002
  # §4.4). Asserts well-formed verify, byte-tampered manifest +
  # signature both rejected, and content-address mismatch (operator
  # passes a wrong --rollout-id) rejected — the rename/swap attack.
  manifestTamperRejectionScenario =
    if nixfleet-verify-artifact == null || rolloutManifestFixture == null
    then
      throw ''
        tests/harness: fleet-harness-manifest-tamper-rejection requires
        both `nixfleet-canonicalize` (for the rolloutManifestFixture)
        and `nixfleet-verify-artifact`. Wire via modules/tests/harness.nix.
      ''
    else
      import ./scenarios/manifest-tamper-rejection.nix {
        inherit pkgs rolloutManifestFixture;
        verifyArtifactPkg = nixfleet-verify-artifact;
      };

  # Module-rollouts-wire scenario. Boots the actual NixOS service
  # module (modules/scopes/nixfleet/_control-plane.nix) with
  # `rolloutsDir` configured, and asserts the running CP serves the
  # fixture's manifest pair at GET /v1/rollouts/<id>{,/sig}. Catches
  # ExecStart-construction regressions in the module that unit tests
  # + the auditor / agent-side scenarios cannot.
  moduleRolloutsWireScenario =
    if nixfleet-control-plane == null || rolloutManifestFixture == null
    then
      throw ''
        tests/harness: fleet-harness-module-rollouts-wire requires both
        `nixfleet-control-plane` and `nixfleet-canonicalize` (for the
        rolloutManifestFixture) to be passed in. Wire via
        modules/tests/harness.nix.
      ''
    else
      import ./scenarios/module-rollouts-wire.nix {
        inherit lib pkgs inputs rolloutManifestFixture signedFixture;
        testCerts = sharedCerts;
        cpPkg = nixfleet-control-plane;
      };

  # Deadline-expiry scenario. Real CP with a 3-second
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

  # Rollback-policy hardware harness scenario (#76). Real CP + real
  # agent; injects a Failed row in host_rollout_state under a
  # `rollback-and-halt` fleet variant and walks the wire round-trip
  # end-to-end (CP rollback_signal → agent rollback handler →
  # RollbackTriggered post → Reverted transition → idempotent stop).
  rollbackPolicyScenario =
    if nixfleet-control-plane == null || nixfleet-agent == null
    then
      throw ''
        tests/harness: fleet-harness-rollback-policy requires both
        `nixfleet-control-plane` and `nixfleet-agent`. Wire via
        modules/tests/harness.nix.
      ''
    else let
      # Rollback-and-halt variant of the convergence-paired signed
      # fixture. Same closureHash + seedSalt shape as the converged
      # variant so the agent's reported closure matches; only
      # `onHealthFailure` flips, which is what unlocks
      # `compute_rollback_signal`.
      rollbackHaltSignedFixture =
        if nixfleet-canonicalize == null
        then null
        else
          import ./fixtures/signed {
            inherit lib pkgs nixfleet-canonicalize;
            hostClosureHashes = {
              "agent-01" = convergedClosureHash;
              "agent-02" = convergedClosureHash;
            };
            onHealthFailure = "rollback-and-halt";
            derivationName = "nixfleet-harness-signed-fixture-rollback-halt";
          };
    in
      if rollbackHaltSignedFixture == null
      then
        throw ''
          tests/harness: fleet-harness-rollback-policy requires
          `nixfleet-canonicalize` for the rollback-halt fixture.
        ''
      else
        import ./scenarios/rollback-policy.nix (scenarioArgs
          // {
            signedFixture = rollbackHaltSignedFixture;
            closureHash = convergedClosureHash;
            cpPkg = nixfleet-control-plane;
            agentPkg = nixfleet-agent;
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

  fleet-harness-corruption-rejection = corruptionRejectionScenario;

  fleet-harness-future-dated-rejection = futureDatedRejectionScenario;

  fleet-harness-enroll-replay = enrollReplayScenario;

  fleet-harness-manifest-tamper-rejection = manifestTamperRejectionScenario;

  fleet-harness-module-rollouts-wire = moduleRolloutsWireScenario;

  fleet-harness-secret-hygiene = secretHygieneScenario;

  fleet-harness-rollback-policy = rollbackPolicyScenario;

  # Fleet-N variants. fleet-2 is identical to smoke
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
  inherit revocationsFixture;
  inherit rolloutManifestFixture;
}
