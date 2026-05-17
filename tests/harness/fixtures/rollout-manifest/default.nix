{
  pkgs,
  nixfleet-canonicalize,
  seedSalt ? "nixfleet-harness-test-seed-2026",
  signedAt ? "2026-05-01T00:00:00Z",
  derivationName ? "nixfleet-harness-rollout-manifest-fixture",
}: let
  signBytes = import ../signed/sign-bytes.nix;

  manifestPayload = {
    schemaVersion = 1;
    displayName = "stable@def4567";
    channel = "stable";
    channelRef = "def4567abc123def4567abc123def4567abc123d";
    fleetResolvedHash = "1111111111111111111111111111111111111111111111111111111111111111";
    hostSet = [
      {
        hostname = "agent-01";
        waveIndex = 0;
        targetClosure = "0000000000000000000000000000000000000000-host-a";
      }
      {
        hostname = "agent-02";
        waveIndex = 1;
        targetClosure = "1111111111111111111111111111111111111111-host-b";
      }
    ];
    healthGate = {};
    meta = {
      schemaVersion = 1;
      signedAt = signedAt;
      ciCommit = "def45678";
      signatureAlgorithm = "ed25519";
    };
  };

  signed = signBytes {
    inherit pkgs nixfleet-canonicalize seedSalt;
    name = "${derivationName}-signed";
    jsonContent = builtins.toJSON manifestPayload;
  };
in
  pkgs.runCommand derivationName {
    nativeBuildInputs = [pkgs.coreutils];
  } ''
    set -euo pipefail
    mkdir -p "$out"

    cp "${signed}/canonical.json"      "$out/manifest.canonical.json"
    cp "${signed}/canonical.json.sig"  "$out/manifest.canonical.json.sig"
    cp "${signed}/pubkey.b64"          "$out/pubkey.b64"

    # RFC-0012 §6.3 canonical RolloutId: "{channel}@{channel_ref}". The
    # CP routes + agent parser both expect this shape (D-007). v0.1's
    # content-address form (sha256 of canonical bytes) was retired with
    # the RolloutId newtype unification.
    printf '%s@%s' '${manifestPayload.channel}' '${manifestPayload.channelRef}' > "$out/rollout-id"

    pubkey=$(cat "$out/pubkey.b64")
    cat > "$out/trust.json" <<EOF
    {
      "schemaVersion": 1,
      "ciReleaseKey": {
        "current": { "algorithm": "ed25519", "public": "$pubkey" },
        "previous": null,
        "rejectBefore": null
      },
      "cacheKeys": [],
      "orgRootKey": null
    }
    EOF

    printf '%s' '${signedAt}' > "$out/signed-at"
  ''
