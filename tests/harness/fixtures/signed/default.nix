{
  lib,
  pkgs,
  nixfleet-canonicalize,
  mkFleetPath ? ../../../../lib/mk-fleet.nix,
  signedAt ? "2026-05-01T00:00:00Z",
  freshnessWindowMinutes ? 86400,
  seedSalt ? "nixfleet-harness-test-seed-2026",
  derivationName ? "nixfleet-harness-signed-fixture",
  hostClosureHashes ? {},
  onHealthFailure ? "halt",
  # OpenSSH-format public keys per agent, e.g.
  # `{ "agent-01" = "ssh-ed25519 AAAA..."; "agent-99" = "ssh-ed25519 ..."; }`.
  # The CP binds agent CSRs (/v1/enroll) and last_confirmed_at
  # attestations against the host's declared pubkey per #43. Entries
  # for the built-in harness hosts (agent-01, agent-02, cp) override
  # their pubkey field; entries for unknown hostnames add a new host
  # to the fleet with that pubkey.
  agentPubkeys ? {},
}: let
  fixedSignedAt = signedAt;
  fixedCiCommit = "0000000000000000000000000000000000000000";
  fixedAlgorithm = "ed25519";

  # LOADBEARING: mkFleet requires each host to carry config.system.build.toplevel.
  stubConfiguration = {
    config.system.build.toplevel = {
      outPath = "/nix/store/0000000000000000000000000000000000000000-stub";
      drvPath = "/nix/store/0000000000000000000000000000000000000000-stub.drv";
    };
  };

  mkFleetImpl = import mkFleetPath {inherit lib;};
  inherit (mkFleetImpl) mkFleet withSignature;

  # trailing newline from builtins.readFile would break the OpenSSH parser
  stripTrailingNewline = s: lib.removeSuffix "\n" s;
  pubkeyFor = name: lib.mapNullable stripTrailingNewline (agentPubkeys.${name} or null);

  baseHosts = {
    agent-01 = {
      platform = "x86_64-linux";
      configuration = stubConfiguration;
      tags = ["harness"];
      channel = "stable";
      pubkey = pubkeyFor "agent-01";
    };
    agent-02 = {
      platform = "x86_64-linux";
      configuration = stubConfiguration;
      tags = ["harness"];
      channel = "stable";
      pubkey = pubkeyFor "agent-02";
    };
    cp = {
      platform = "x86_64-linux";
      configuration = stubConfiguration;
      tags = ["harness" "control-plane"];
      channel = "stable";
      pubkey = pubkeyFor "cp";
    };
  };

  # Any agentPubkeys entries for hostnames not in baseHosts add new
  # hosts to the fleet. Lets enroll-replay add `agent-99` without
  # tracking it explicitly in this file.
  extraHosts = lib.mapAttrs (_name: openssh: {
    platform = "x86_64-linux";
    configuration = stubConfiguration;
    tags = ["harness"];
    channel = "stable";
    pubkey = stripTrailingNewline openssh;
  }) (lib.filterAttrs (n: _: !(baseHosts ? ${n})) agentPubkeys);

  fleetInput = {
    hosts = baseHosts // extraHosts;
    channels.stable = {
      description = "Harness signed-fixture channel.";
      rolloutPolicy = "all-at-once";
      reconcileIntervalMinutes = 30;
      signingIntervalMinutes = 60;
      freshnessWindow = freshnessWindowMinutes;
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
      inherit onHealthFailure;
    };
    edges = [];
    disruptionBudgets = [];
  };

  fleet = mkFleet fleetInput;

  resolvedWithClosureHashes =
    fleet.resolved
    // {
      hosts = lib.mapAttrs (name: host:
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

  # Per-channel rollout manifest projection (mirrors
  # `nixfleet_reconciler::project_manifest`). Produces one
  # `<channel>@<channel_ref>.json` per channel that has at least one
  # host with a closureHash. Mounted by `cp-real.nix` via a local
  # HTTP server so CP's `manifest_poll` can fetch + verify them.
  shortRef = builtins.substring 0 8 fixedCiCommit;
  allHosts = baseHosts // extraHosts;
  hostsOnChannel = channelName:
    lib.filterAttrs (
      hostname: _host:
        (allHosts.${hostname}.channel or null)
        == channelName
        && hostClosureHashes ? ${hostname}
    )
    allHosts;
  channelsWithClosures =
    lib.filter (ch: (hostsOnChannel ch) != {})
    (lib.attrNames fleetInput.channels);
  manifestPayloadFor = channelName: let
    matched = hostsOnChannel channelName;
    policyName = fleetInput.channels.${channelName}.rolloutPolicy;
    policy = fleetInput.rolloutPolicies.${policyName};
    healthGate = policy.healthGate or {};
  in {
    schemaVersion = 1;
    displayName = "${channelName}@${shortRef}";
    channel = channelName;
    channelRef = fixedCiCommit;
    fleetResolvedHash = "__FLEET_RESOLVED_HASH__";
    hostSet =
      lib.sort (a: b: a.hostname < b.hostname)
      (lib.mapAttrsToList (hostname: _host: {
          inherit hostname;
          waveIndex = 0;
          targetClosure = hostClosureHashes.${hostname};
        })
        matched);
    inherit healthGate;
    disruptionBudgets = [];
    meta = {
      schemaVersion = 1;
      signedAt = fixedSignedAt;
      ciCommit = fixedCiCommit;
      signatureAlgorithm = fixedAlgorithm;
    };
  };
  manifestPayloads =
    lib.genAttrs channelsWithClosures manifestPayloadFor;
  rolloutIds =
    lib.genAttrs channelsWithClosures
    (ch: "${ch}@${fixedCiCommit}");
  signedRollouts =
    if channelsWithClosures == []
    then null
    else
      import ./sign-rollout-manifests.nix {
        inherit pkgs lib nixfleet-canonicalize seedSalt;
        name = "${derivationName}-rollouts-signed";
        fleetCanonicalJson = "${signed}/canonical.json";
        inherit manifestPayloads rolloutIds;
      };

  # FOOTGUN: signedAt+1h via literal string replace (Nix lacks chrono parser); asserts midnight suffix to avoid silent wrong now.
  signedAtMidnightSuffix = "T00:00:00Z";
  signedAtPlusHourSuffix = "T01:00:00Z";
  now = assert lib.hasSuffix signedAtMidnightSuffix signedAt;
    lib.removeSuffix signedAtMidnightSuffix signedAt + signedAtPlusHourSuffix;
in
  pkgs.runCommand derivationName {
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

    mkdir -p "$out/rollouts"
    ${lib.optionalString (signedRollouts != null) ''
      cp -r ${signedRollouts}/. "$out/rollouts/"
    ''}
  ''
